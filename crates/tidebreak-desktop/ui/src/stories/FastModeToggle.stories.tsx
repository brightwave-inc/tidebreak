import type { Meta, StoryObj } from "@storybook/react-vite";

import { FastModeToggle } from "@/code/CodeComposer";

/**
 * The code composer's fast-mode switch.
 *
 * Fast mode runs the same model faster and charges more per token, so this is
 * the one footer control that changes what a turn costs without changing what
 * it does. It wears the same violet treatment as the top effort rung, because
 * the two say the same thing to someone scanning the footer: this turn is more
 * expensive than the default.
 *
 * Availability is per model, not per engine. Anthropic serves fast mode on
 * part of the Opus line only, and Codex advertises the tier per catalog row,
 * so a model without it renders no control at all rather than a disabled one.
 * A dead toggle would invite the question of how to enable it; nothing at all
 * matches how the effort control already handles an empty ladder.
 */
const meta = {
  title: "Composer/Fast mode",
  component: FastModeToggle,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 p-10">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof FastModeToggle>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The resting state on a model that serves the tier. */
export const Off: Story = {
  args: {
    available: true,
    value: false,
    onChange: () => {},
  },
};

/** Armed. The violet treatment marks the premium while it is in force. */
export const On: Story = {
  args: {
    available: true,
    value: true,
    onChange: () => {},
  },
};

/**
 * A model that does not serve fast mode renders nothing, so this story is
 * deliberately an empty frame. It is here so the absence is reviewable: a
 * regression that renders a disabled toggle instead shows up as a control
 * appearing where the story expects none.
 */
export const Unavailable: Story = {
  args: {
    available: false,
    value: false,
    onChange: () => {},
  },
};

/** Armed but frozen, which is how it reads while a turn is running. */
export const Disabled: Story = {
  args: {
    available: true,
    value: true,
    disabled: true,
    onChange: () => {},
  },
};
