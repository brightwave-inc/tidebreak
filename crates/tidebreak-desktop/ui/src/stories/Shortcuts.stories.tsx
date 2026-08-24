import type { Meta, StoryObj } from "@storybook/react-vite";

import { ShortcutsList } from "@/ShortcutsDialog";

/**
 * The keyboard-shortcut help, drawn from the same table the listener matches
 * on.
 *
 * The list is per mode because one chord can mean two things: Cmd+N is a
 * conversation in chat and a workspace in code. `command` is fixed per story
 * rather than read from the browser, so the keycaps look the same wherever the
 * story is opened.
 */
const meta = {
  title: "Navigation/Keyboard shortcuts",
  component: ShortcutsList,
  args: { command: true },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-md rounded-xl border border-border-subtle bg-background p-6">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ShortcutsList>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * Code mode: the Ship group carries a branch from commit to merged without the
 * mouse, and the frame chords sit under it.
 */
export const CodeMode: Story = {
  args: { mode: "code" },
};

/** Chat mode, where the Ship and Code groups have nothing to list. */
export const ChatMode: Story = {
  args: { mode: "chat" },
};

/** The same table on a keyboard whose modifier is Ctrl rather than Cmd. */
export const WindowsKeycaps: Story = {
  args: { mode: "code", command: false },
};
