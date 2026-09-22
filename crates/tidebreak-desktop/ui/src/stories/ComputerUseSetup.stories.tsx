import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import { ComputerUseSetupDialog } from "@/ComputerUseSetupDialog";
import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "@/computerUsePermissions";

const missing: ComputerUsePermissionStatus = {
  status: "available",
  appName: "Tidebreak",
  appIdentifier: "io.brightwave.tidebreak",
  screenRecording: false,
  accessibility: false,
};
const localHost = (
  status: ComputerUsePermissionStatus,
): ComputerUsePermissionHost => ({
  availability: () => "local",
  status: fn(async () => status),
  request: fn(async () => status),
  openSettings: fn(async () => {}),
});

/** The dialog portals to `document.body`, so stories query the screen. */
const body = (canvasElement: HTMLElement) =>
  within(canvasElement.ownerDocument.body);

const meta = {
  title: "Modes/Computer use setup",
  component: ComputerUseSetupDialog,
  parameters: { layout: "fullscreen" },
  args: { open: true, host: localHost(missing), onDone: fn() },
} satisfies Meta<typeof ComputerUseSetupDialog>;
export default meta;
type Story = StoryObj<typeof meta>;

/** The first launch on a Mac that has granted neither permission. */
export const Missing: Story = {
  play: async ({ canvasElement }) => {
    await body(canvasElement).findByRole("button", { name: "Allow" });
  },
};

/** One grant already held, which is what a partial earlier setup leaves. */
export const PartlyAllowed: Story = {
  args: { host: localHost({ ...missing, screenRecording: true }) },
  play: async ({ canvasElement }) => {
    await body(canvasElement).findByText("Allowed");
  },
};

/** Both grants landed: the ask collapses to a single confirmation. */
export const Ready: Story = {
  args: {
    host: localHost({ ...missing, screenRecording: true, accessibility: true }),
  },
  play: async ({ canvasElement }) => {
    await body(canvasElement).findByRole("button", { name: "Done" });
  },
};

/** The helper could not raise the macOS modals. The ask stays spendable. */
export const RequestFailure: Story = {
  args: {
    host: {
      ...localHost(missing),
      request: fn(async () => {
        throw new Error("request failed");
      }),
    },
  },
  play: async ({ canvasElement }) => {
    const canvas = body(canvasElement);
    await userEvent.click(await canvas.findByRole("button", { name: "Allow" }));
    await expect(canvas.findByRole("alert")).resolves.toHaveTextContent(
      "permissions could not be requested",
    );
  },
};
