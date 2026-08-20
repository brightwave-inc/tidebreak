import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { WorktreeRootSection } from "@/settings/WorktreeRootSection";

/**
 * Where new workspaces put their worktrees. The states that matter are whether
 * the folder is the default or one the user chose, and whether a save is in
 * flight — a reader has to be able to tell an inherited path from a decision.
 */
const meta = {
  title: "Settings/Workspace folder",
  component: WorktreeRootSection,
  args: {
    value: "",
    effectiveRoot: "/Users/sam/Tidebreak/workspaces",
    defaultRoot: "/Users/sam/Tidebreak/workspaces",
    inherited: true,
    busy: false,
    canBrowse: true,
    onChange: fn(),
    onBrowse: fn(),
    onSave: fn(),
    onReset: fn(),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof WorktreeRootSection>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing chosen yet: the default is in force and Reset has nothing to do. */
export const Default: Story = {};

/** A folder the user picked, with Reset live. */
export const CustomFolder: Story = {
  args: {
    value: "/Volumes/work/trees",
    effectiveRoot: "/Volumes/work/trees",
    inherited: false,
  },
};

/** An edited draft, before it is saved. */
export const Edited: Story = {
  args: {
    value: "/Volumes/work/trees-2",
    effectiveRoot: "/Volumes/work/trees",
    inherited: false,
  },
};

/** Mid-save: every control is inert until the server answers. */
export const Saving: Story = {
  args: {
    value: "/Volumes/work/trees",
    effectiveRoot: "/Users/sam/Tidebreak/workspaces",
    inherited: true,
    busy: true,
  },
};

/** A browser session, or a headless deployment: no native folder picker. */
export const WithoutPicker: Story = {
  args: { canBrowse: false },
};

/** A headless deployment, where the default stays inside the data directory. */
export const HeadlessDefault: Story = {
  args: {
    effectiveRoot: "/srv/tidebreak/code/worktrees",
    defaultRoot: "/srv/tidebreak/code/worktrees",
  },
};
