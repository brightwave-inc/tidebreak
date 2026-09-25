import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { ConnectionNotice } from "@/ConnectionNotice";

/**
 * A lost connection to the server, above the composer in a conversation and
 * in a code session alike.
 *
 * A drop starts quiet, because most come back within seconds. After 30
 * seconds it becomes a notice with Retry now, and on another machine with a
 * way back to this computer. A local server that stopped after it started
 * says so and offers a restart, since nothing reconnects to it on its own.
 */
const meta = {
  title: "Composer/Connection notice",
  component: ConnectionNotice,
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl p-10">
        <Story />
      </div>
    ),
  ],
  args: {
    state: "reconnecting",
    onRetryNow: fn(async () => {}),
    onWorkLocally: fn(async () => {}),
    onRestart: fn(async () => {}),
  },
} satisfies Meta<typeof ConnectionNotice>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The first 30 seconds of a drop: a quiet line, and nothing to do yet. */
export const Reconnecting: Story = {};

/** Still down after 30 seconds, on this computer's own server. */
export const NotAnswering: Story = {
  args: { state: "escalated" },
};

/** Still down after 30 seconds, on another machine, with the way back. */
export const RemoteNotAnswering: Story = {
  args: { state: "escalated", machine: "tidebreak.example.com" },
};

/** The local server's accept loop died: only a restart brings it back. */
export const ServerStopped: Story = {
  args: { state: "stopped" },
};

/** The remote notice in a narrow pane beside an open panel. */
export const RemoteNotAnsweringNarrow: Story = {
  args: {
    state: "escalated",
    machine: "tidebreak.some-quite-long-internal-hostname.example.com",
  },
  decorators: [
    (Story) => (
      <div className="max-w-sm p-4">
        <Story />
      </div>
    ),
  ],
};
