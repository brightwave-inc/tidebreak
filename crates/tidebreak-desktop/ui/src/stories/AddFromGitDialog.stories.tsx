import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { AddFromGitDialogView } from "@/plugins/AddFromGitDialog";

const meta = {
  title: "Plugins/Add from Git",
  component: AddFromGitDialogView,
  parameters: { layout: "centered" },
  args: {
    open: true,
    url: "",
    revision: "",
    phase: { status: "empty" },
    onUrlChange: fn(),
    onRevisionChange: fn(),
    onSubmit: fn(),
    onOpenChange: fn(),
  },
} satisfies Meta<typeof AddFromGitDialogView>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {};

export const Validating: Story = {
  args: {
    url: "https://github.com/acme/notes",
    revision: "v1.0.0",
    phase: { status: "validating" },
  },
};

export const Installing: Story = {
  args: {
    url: "https://github.com/acme/notes",
    revision: "v1.0.0",
    phase: { status: "installing" },
  },
};

export const InstalledWithSkippedMembers: Story = {
  args: {
    url: "https://github.com/acme/notes",
    revision: "v1.0.0",
    phase: {
      status: "installed",
      outcome: {
        plugin: "meeting-notes",
        revision: "v1.0.0",
        skipped: [
          {
            path: "skills/draft/SKILL.md",
            reason: "The skill name is not a valid slug",
          },
          {
            path: "skills/broken/SKILL.md",
            reason: "The skill description is missing",
          },
        ],
      },
    },
  },
};

export const Failed: Story = {
  args: {
    url: "https://github.com/acme/notes",
    revision: "v1.0.0",
    phase: {
      status: "failed",
      message: "No recognizable plugin was found in that repository.",
    },
  },
};
