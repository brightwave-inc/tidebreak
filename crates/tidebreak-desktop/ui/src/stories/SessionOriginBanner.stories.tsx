import type { Meta, StoryObj } from "@storybook/react-vite";

import { SessionOriginBanner } from "@/code/SessionOriginBanner";

/** Reuse the session banner for both machine and sandbox execution. */
const meta = {
  title: "Code/Session origin banner",
  component: SessionOriginBanner,
  args: { executionLocation: "machine" },
  decorators: [
    (Story) => (
      <div className="bg-background w-[42rem] max-w-full pb-4">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof SessionOriginBanner>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A channel thread: the key yields a permalink, so the link renders. */
export const SlackThread: Story = {
  args: {
    origin: {
      channel_kind: "slack",
      external_key: "T0400000:C0812345:1724900000.123456",
    },
  },
};

/** A DM generation key has no thread timestamp, so no link renders. */
export const SlackDirectMessage: Story = {
  args: {
    origin: {
      channel_kind: "slack",
      external_key: "T0400000:D0898765:dm2",
    },
  },
};

/** An unrecognized channel family falls back to its raw kind, linkless. */
export const OtherChannel: Story = {
  args: {
    origin: {
      channel_kind: "matrix",
      external_key: "!room:example.org",
    },
  },
};

export const SandboxThread: Story = {
  args: { ...SlackThread.args, executionLocation: "sandbox" },
};
