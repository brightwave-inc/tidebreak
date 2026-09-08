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

/** A person session borrows the caller's own forge identity. */
export const ActsAsYou: Story = {
  args: { ...SlackThread.args, actsAs: "person" },
};

/** A service session borrows the App's bot. */
export const ActsAsTheBot: Story = {
  args: { ...SlackThread.args, actsAs: "bot" },
};

export const SeveralThreads: Story = {
  args: {
    ...ActsAsTheBot.args,
    origins: [
      SlackThread.args.origin,
      {
        channel_kind: "slack",
        external_key: "T0400000:C0865432:1724900010.123456",
      },
      {
        channel_kind: "slack",
        external_key: "T0400000:C0898765:1724900020.123456",
      },
    ],
  },
};

export const SeveralThreadsNarrow: Story = {
  ...SeveralThreads,
  decorators: [
    (Story) => (
      <div style={{ width: 320 }}>
        <Story />
      </div>
    ),
  ],
};
