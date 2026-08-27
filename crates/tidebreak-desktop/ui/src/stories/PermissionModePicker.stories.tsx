import type { Meta, StoryObj } from "@storybook/react-vite";

import { PermissionModePicker } from "@/code/CodeComposer";
import {
  ALLOW_ALL_NOTE,
  CREATE_PERMISSION_MODE_FIXED,
  UNSUPERVISED_AUTO_NOTE,
} from "@/code/labels";

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

/**
 * Create states Allow under the picker (decision 0039). A live session
 * carries the header chip instead of this line.
 */
export const AllowAllNote: Story = {
  args: {
    value: "allow",
    onChange: () => {},
  },
  render: (args) => (
    <div className="flex min-w-0 max-w-sm flex-col">
      <PermissionModePicker {...args} />
      <p className="text-muted-foreground px-2 text-xs">{ALLOW_ALL_NOTE}</p>
    </div>
  ),
};

/**
 * Auto on an engine with no approval channel (decision 0038). Same placement
 * as Allow: under the control, not inside the menu row.
 */
export const UnsupervisedAutoNote: Story = {
  args: {
    value: "auto",
    onChange: () => {},
  },
  render: (args) => (
    <div className="flex min-w-0 max-w-sm flex-col">
      <PermissionModePicker {...args} />
      <p className="text-muted-foreground px-2 text-xs">
        {UNSUPERVISED_AUTO_NOTE}
      </p>
    </div>
  ),
};
