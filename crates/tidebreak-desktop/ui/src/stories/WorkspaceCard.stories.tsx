import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { workspaceCommands } from "@/code/workspaceActions";
import { WorkspaceCard } from "@/code/WorkspaceCard";
import {
  archivedWorkspace,
  closedPrDigest,
  codeSession,
  codeWorkspace,
  draftPrDigest,
  mergedPrDigest,
  needsYouDigest,
  openPrDigest,
  runningDigest,
  stalledDigest,
  watchDigest,
} from "./fixtures";

/**
 * The rail's workspace card. Hover the card (or tab into it) to swap the
 * state glyphs for the action cluster; right-click for the same commands as
 * a context menu.
 */

const meta = {
  title: "Code/Workspace card",
  component: WorkspaceCard,
  args: {
    workspace: codeWorkspace,
    digest: undefined,
    session: undefined,
    repoName: "tidebreak",
    active: false,
    terminalOpen: false,
    density: "detailed",
    visibleMeta: { repoChip: true, branch: false },
    commands: workspaceCommands({ hasPr: false, archived: false }),
    onOpen: fn(),
    onCommand: fn(),
  },
  decorators: [
    (Story) => (
      <div className="bg-page-background w-[264px] rounded-lg border border-border-subtle p-2 pt-3">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof WorkspaceCard>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Idle: Story = {};

/**
 * The detail panel that opens beside a row on hover (or focus): full branch,
 * state, checks, and the pull request with its action. Rendered open here;
 * on any other story, rest the pointer on the row to see it live.
 */
export const DetailPanel: Story = {
  args: {
    workspace: { ...codeWorkspace, pr: openPrDigest },
    digest: { ...runningDigest, pr_state: openPrDigest },
    session: codeSession,
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
  },
};

/** The panel when the session is waiting on the reader. */
export const DetailPanelNeedsYou: Story = {
  args: {
    digest: needsYouDigest,
    session: codeSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
  },
};

/** How the card reads inside a by-repo group: no repo chip, title leads. */
export const InRepoGroup: Story = {
  args: { visibleMeta: { repoChip: false, branch: false } },
};

/** Detailed view with the branch switched on from the rail settings. */
export const WithBranch: Story = {
  args: { visibleMeta: { repoChip: true, branch: true } },
};

export const Active: Story = {
  args: { active: true },
};

export const RunningSession: Story = {
  args: {
    digest: runningDigest,
    session: codeSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

/** A watch-and-fix task riding under the card as a clickable child row. */
export const WithWatchTask: Story = {
  args: {
    digest: runningDigest,
    session: codeSession,
    childSessions: [watchDigest],
    onOpenChildSession: fn(),
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    workspace: { ...codeWorkspace, pr: openPrDigest },
  },
};

export const NeedsYou: Story = {
  args: {
    digest: needsYouDigest,
    session: codeSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

export const Stalled: Story = {
  args: {
    digest: stalledDigest,
    session: codeSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

export const PullRequestOpen: Story = {
  args: {
    workspace: { ...codeWorkspace, pr: openPrDigest },
    commands: workspaceCommands({ hasPr: true, archived: false }),
  },
};

export const TerminalOpen: Story = {
  args: { terminalOpen: true },
};

/** On the shelf: dimmed row; its panel and menu lead with Restore. */
export const Archived: Story = {
  args: {
    workspace: archivedWorkspace,
    commands: workspaceCommands({ hasPr: false, archived: true }),
    detailDefaultOpen: true,
  },
};

export const LongNames: Story = {
  args: {
    visibleMeta: { repoChip: true, branch: true },
    workspace: {
      ...codeWorkspace,
      title:
        "Rework the gateway credential exchange for hosted machines end to end",
      branch_name:
        "tidebreak/rework-gateway-credential-exchange-for-hosted-machines",
    },
  },
};

/** Every PR tone on one screen: open, draft, merged, closed. */
export const PullRequestTones: Story = {
  render: (args) => (
    <div className="flex flex-col gap-0.5">
      {(
        [
          ["open", openPrDigest],
          ["draft", draftPrDigest],
          ["merged", mergedPrDigest],
          ["closed", closedPrDigest],
        ] as const
      ).map(([tone, pr]) => (
        <WorkspaceCard
          key={tone}
          {...args}
          workspace={{
            ...codeWorkspace,
            id: `ws-${tone}`,
            title: `PR ${tone}`,
            pr,
          }}
          commands={workspaceCommands({ hasPr: true, archived: false })}
        />
      ))}
    </div>
  ),
};

/** A stretch of rail: triage states stacked the way the sidebar shows them. */
export const Rail: Story = {
  render: (args) => (
    <div className="flex flex-col gap-0.5">
      <WorkspaceCard
        {...args}
        workspace={{ ...codeWorkspace, id: "ws-a", title: "Needs a decision" }}
        digest={{ ...needsYouDigest, title: "Needs a decision" }}
        session={codeSession}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
      />
      <WorkspaceCard
        {...args}
        workspace={{
          ...codeWorkspace,
          id: "ws-b",
          title: "Turn in flight",
          branch_name: "tidebreak/turn-in-flight",
        }}
        digest={{ ...runningDigest, title: "Turn in flight" }}
        session={codeSession}
        active
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
      />
      <WorkspaceCard
        {...args}
        workspace={{
          ...codeWorkspace,
          id: "ws-c",
          title: "Shipped, checks green",
          branch_name: "tidebreak/shipped-checks-green",
          pr: openPrDigest,
        }}
        commands={workspaceCommands({ hasPr: true, archived: false })}
      />
      <WorkspaceCard
        {...args}
        workspace={{
          ...codeWorkspace,
          id: "ws-d",
          title: "Parked exploration",
          branch_name: "tidebreak/parked-exploration",
        }}
      />
    </div>
  ),
};
