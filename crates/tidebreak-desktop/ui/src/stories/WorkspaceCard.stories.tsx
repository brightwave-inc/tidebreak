import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { WorkspaceCard } from "@/code/CodeSidebar";
import type { WorkspaceCommand } from "@/code/workspaceActions";
import {
  attentionNeedsYou,
  attentionStalled,
  codeDigest,
  codeSession,
  codeWorkspace,
} from "./fixtures";

const COMMANDS: WorkspaceCommand[] = [
  { id: "open", label: "Open" },
  { id: "rename", label: "Rename…" },
  { id: "copy-branch", label: "Copy branch name" },
  { id: "archive", label: "Archive…", destructive: true, separated: true },
];

/**
 * One workspace on the rail: title + attention dot, repo chip + branch, PR
 * glyph, and a nested session row when something is live. The card carries
 * every state the sidebar must make tellable at a glance.
 */
const meta = {
  title: "Code/Workspace card",
  component: WorkspaceCard,
  args: {
    workspace: codeWorkspace,
    digest: codeDigest(),
    session: codeSession,
    repoName: "tidebreak",
    active: false,
    terminalOpen: false,
    commands: COMMANDS,
    onOpen: fn(),
    onCommand: fn(),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto w-[280px] pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof WorkspaceCard>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A session is running; the nested row narrates it. */
export const Working: Story = {};

export const NeedsYou: Story = {
  args: {
    digest: codeDigest({ attention: attentionNeedsYou, lifecycle: "idle" }),
  },
};

export const Stalled: Story = {
  args: {
    digest: codeDigest({ attention: attentionStalled }),
  },
};

/** No live session: just identity, quiet. */
export const Idle: Story = {
  args: { digest: undefined, session: undefined },
};

export const ActiveWithTerminal: Story = {
  args: { active: true, terminalOpen: true },
};

export const WithPullRequest: Story = {
  args: {
    digest: codeDigest({
      pr_state: {
        number: 184,
        url: "https://github.com/example/tidebreak/pull/184",
        state: "open",
        checks_summary: "7 passing, 1 failing",
      },
    }),
  },
};

/** Long names must truncate, never wrap or push the glyphs out. */
export const LongNames: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      title: "Rework the entire onboarding flow for multi-repo workspaces",
      branch_name:
        "tidebreak/rework-the-entire-onboarding-flow-for-multi-repo-workspaces",
    },
    digest: codeDigest({
      title: "Rework the entire onboarding flow for multi-repo workspaces",
    }),
  },
};
