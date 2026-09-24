import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import { ComputerUsePermissionDialog } from "@/ComputerUsePermissionDialog";
import { useComputerUsePermissionAsk } from "@/computerUsePermissionAsk";
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
  restart: fn(async () => {}),
});

/** The dialog portals to `document.body`, so stories query the screen. */
const body = (canvasElement: HTMLElement) =>
  within(canvasElement.ownerDocument.body);

/** Pretend macOS was already asked for Screen Recording in this run. */
function screenRecordingRequested() {
  useComputerUsePermissionAsk.setState({ screenRecordingRequested: true });
  return () =>
    useComputerUsePermissionAsk.setState({ screenRecordingRequested: false });
}

const meta = {
  title: "Modes/Computer use permission ask",
  component: ComputerUsePermissionDialog,
  parameters: { layout: "fullscreen" },
  // The ask's memory is app-wide; each story starts from a fresh launch.
  beforeEach: () => {
    useComputerUsePermissionAsk.setState({
      ask: null,
      need: null,
      screenRecordingRequested: false,
    });
  },
  args: {
    ask: { subject: "apps", forTask: true },
    host: localHost(missing),
    onClose: fn(),
    onOpenSettings: fn(),
  },
} satisfies Meta<typeof ComputerUsePermissionDialog>;
export default meta;
type Story = StoryObj<typeof meta>;

/**
 * The first time a task needs to see or use another app. Not now holds focus,
 * so Enter cannot raise the macOS prompts.
 */
export const ComputerUse: Story = {
  play: async ({ canvasElement }) => {
    const canvas = body(canvasElement);
    await canvas.findAllByText("Not allowed");
    await expect(canvas.getByRole("button", { name: "Not now" })).toHaveFocus();
  },
};

/** A task working in a browser's own window, worded for the browser. */
export const BrowserControl: Story = {
  args: {
    ask: { subject: "browser", forTask: true },
    host: localHost({ ...missing, screenRecording: true }),
  },
  play: async ({ canvasElement }) => {
    await body(canvasElement).findByText("Allowed");
  },
};

/**
 * Screen Recording was requested in this run and still reads off: macOS
 * applies it only after a restart, so the ask offers one.
 */
export const RestartToApply: Story = {
  args: { host: localHost({ ...missing, accessibility: true }) },
  beforeEach: screenRecordingRequested,
  play: async ({ canvasElement }) => {
    await body(canvasElement).findByRole("button", {
      name: "Restart Tidebreak",
    });
  },
};

/** Both permissions landed: the ask collapses to a single confirmation. */
export const Ready: Story = {
  args: {
    host: localHost({ ...missing, screenRecording: true, accessibility: true }),
  },
  play: async ({ canvasElement }) => {
    await body(canvasElement).findByRole("button", { name: "Done" });
  },
};

/** The helper could not raise the macOS prompts. The ask stays open. */
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
