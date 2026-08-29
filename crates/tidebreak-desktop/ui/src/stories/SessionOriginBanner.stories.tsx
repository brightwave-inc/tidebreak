import type { Meta, StoryObj } from "@storybook/react-vite";

import { SessionOriginBanner } from "@/code/SessionOriginBanner";

/**
 * The provenance banner for a session an external channel created. Its
 * journal is coarse on purpose — the banner says why, and links back to the
 * thread when the origin key carries one. A key without a derivable
 * permalink still gets the banner, just without the link.
 */
const meta = {
  title: "Code/Session origin banner",
  component: SessionOriginBanner,
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
      external_key: "T0400000:D0898765:dm:2",
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
