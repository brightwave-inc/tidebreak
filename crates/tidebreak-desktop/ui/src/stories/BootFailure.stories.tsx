import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { BootFailure } from "@/BootFailure";

/**
 * The screen the shell falls back to when it cannot reach the API it is
 * attached to.
 *
 * Worth a story because it is otherwise only reachable by breaking the
 * network: the states below are the ones that decide whether a reader can get
 * themselves out of it, and they differ mostly in which recovery is on offer.
 */
const meta = {
  title: "Shell/Boot failure",
  component: BootFailure,
  parameters: { layout: "fullscreen" },
  args: {
    stage: "catalog",
    error: new TypeError("Load failed"),
    appVersion: "0.58.0",
    onRetry: fn(),
    onWorkLocally: fn(async () => {}),
    writeClipboard: fn(async () => {}),
    attachment: {
      attachment: "remote",
      baseUrl: "https://tidebreak.example.com",
      gatewayAuth: true,
    },
  },
} satisfies Meta<typeof BootFailure>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * The state this screen exists for: the window is attached to another machine
 * that stopped answering, so the reader is offered a way back to this one.
 */
export const UnreachableRemoteMachine: Story = {};

/**
 * The embedded server failed to start. Nothing to detach from, so retry is the
 * only recovery — and the reader still gets the error and a way to report it.
 */
export const LocalServerFailed: Story = {
  args: {
    stage: "connect",
    error: new Error("another instance already owns this data directory"),
    attachment: { attachment: "local", baseUrl: null, gatewayAuth: false },
  },
};

/**
 * The connection resolved but the catalog did not. The machine is local, so
 * the headline names the step rather than an address.
 */
export const LocalCatalogFailed: Story = {
  args: {
    error: new Error("500: could not read the model catalog"),
    attachment: { attachment: "local", baseUrl: null, gatewayAuth: false },
  },
};

/**
 * The shell could not even read its own attachment — the boot screen degrades
 * to the generic sentence rather than claiming a machine it cannot name.
 */
export const AttachmentUnknown: Story = {
  args: { stage: "connect", attachment: null },
};

/**
 * A long address and a long error, which is the realistic shape of a failure
 * worth copying. Pins that neither one breaks the layout.
 */
export const LongDetail: Story = {
  args: {
    attachment: {
      attachment: "remote",
      baseUrl: "https://tidebreak.some-quite-long-internal-hostname.example.com",
      gatewayAuth: true,
    },
    error: new Error(
      "TypeError: Load failed — the machine did not answer within the " +
        "request timeout, and no response headers were received",
    ),
  },
};
