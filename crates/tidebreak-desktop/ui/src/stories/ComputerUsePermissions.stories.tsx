import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";
import type {
  ComputerUsePermissionHost,
  ComputerUsePermissionStatus,
} from "@/computerUsePermissions";
import { ComputerUsePermissionsSection } from "@/settings/ComputerUsePermissionsSection";
import { SettingsPanel } from "@/settings/primitives";

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
const meta = {
  title: "Settings/Computer use permissions",
  component: ComputerUsePermissionsSection,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      <SettingsPanel
        title="Permissions"
        description="Manage what Tidebreak can access."
      >
        <Story />
      </SettingsPanel>
    ),
  ],
  args: { host: localHost(missing) },
} satisfies Meta<typeof ComputerUsePermissionsSection>;
export default meta;
type Story = StoryObj<typeof meta>;

export const Missing: Story = {
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("button", {
      name: "Request macOS permissions",
    });
  },
};
export const PartlyAllowed: Story = {
  args: { host: localHost({ ...missing, screenRecording: true }) },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText("Allowed");
  },
};
export const Ready: Story = {
  args: {
    host: localHost({ ...missing, screenRecording: true, accessibility: true }),
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText(/macOS permissions are ready/);
  },
};
export const Loading: Story = {
  args: {
    host: {
      ...localHost(missing),
      status: fn(() => new Promise<ComputerUsePermissionStatus>(() => {})),
    },
  },
};
export const StatusFailure: Story = {
  args: {
    host: {
      ...localHost(missing),
      status: fn(async () => {
        throw new Error("helper unavailable");
      }),
    },
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("alert");
  },
};
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
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Request macOS permissions" }),
    );
    await expect(canvas.findByRole("alert")).resolves.toHaveTextContent(
      "permissions could not be requested",
    );
  },
};
export const Remote: Story = {
  args: { host: { ...localHost(missing), availability: () => "remote" } },
};
export const Unsupported: Story = {
  args: { host: localHost({ status: "unsupported" }) },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText(/requires the macOS desktop app/);
  },
};
export const LongAppName: Story = {
  args: {
    host: localHost({
      ...missing,
      appName: "Tidebreak development and acceptance testing",
      appIdentifier:
        "io.brightwave.tidebreak.wkacceptance.local-development-profile-for-native-computer-use",
    }),
  },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByRole("button", {
      name: "Request macOS permissions",
    });
  },
};
