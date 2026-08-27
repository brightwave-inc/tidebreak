import type { Meta, StoryObj } from "@storybook/react-vite";

import { PermissionModePicker } from "@/code/CodeComposer";
import { CREATE_PERMISSION_MODE_FIXED } from "@/code/labels";

/**
 * Permission mode in the composer footer.
 *
 * Most engines take a new mode on the next turn. opencode fixes its posture
 * when the session is created, so after the first turn the trigger stays
 * visible, disabled, with the locked-mode tooltip. Create surfaces keep the
 * picker live and add a short hint so Plan vs Allow is an informed choice.
 */
const meta = {
  title: "Composer/Permission mode",
  component: PermissionModePicker,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 p-10">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof PermissionModePicker>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A live session whose engine can still change mode. */
export const Live: Story = {
  args: {
    value: "ask",
    onChange: () => {},
  },
};

/**
 * After opencode has started: the current mode is shown, the trigger is
 * disabled, and the tooltip explains that a new session is the way to
 * pick a different one.
 */
export const LockedAfterStart: Story = {
  args: {
    value: "allow",
  },
};

/** Create-time hint next to a still-live picker. */
export const FixedAtCreate: Story = {
  args: {
    value: "allow",
    onChange: () => {},
  },
  render: (args) => (
    <div className="flex min-w-0 flex-col">
      <PermissionModePicker {...args} />
      <p className="text-muted-foreground px-2 text-xs">
        {CREATE_PERMISSION_MODE_FIXED}
      </p>
    </div>
  ),
};
