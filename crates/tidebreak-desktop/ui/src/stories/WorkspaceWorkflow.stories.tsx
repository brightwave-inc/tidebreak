import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import type { CodeWorkspacePrSnapshot } from "@/api";
import { WorkspaceWorkflowControl } from "@/code/WorkspaceWorkflowControl";
import type {
  CodeWorkspacePrMutation,
  CodeWorkspacePrResource,
} from "@/code/useCodeWorkspacePr";
import {
  blockedWatchPrGit,
  dirtyGit,
  fixingPrGit,
  openPrGit,
  readyForPrGit,
  watchingPrGit,
} from "./fixtures";

function resourceFor(
  snapshot: CodeWorkspacePrSnapshot | null,
  busy: CodeWorkspacePrMutation | null = null,
): CodeWorkspacePrResource {
  return {
    data: snapshot,
    error: null,
    refreshing: false,
    refresh: async () => {},
    adopt: () => {},
    busy,
    mutationError: null,
    setMutationError: () => {},
    refreshFromHost: async () => undefined,
    runMutation: async () => undefined,
  };
}

function WorkflowState({
  snapshot,
  busy = null,
  watchTaskLink = false,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  busy?: CodeWorkspacePrMutation | null;
  /** Offer the "Watching in …" link the workspace page wires up. */
  watchTaskLink?: boolean;
}) {
  return (
    <WorkspaceWorkflowControl
      client={{
        pushCodeWorkspace: fn(),
        createCodePullRequest: fn(),
        startCodeWatch: fn(),
        stopCodeWatch: fn(),
      }}
      workspaceId="ws-1"
      branchName="tidebreak/scoped-ui-workshop"
      baseRef="main"
      resource={resourceFor(snapshot, busy)}
      onOpenSourceControl={fn()}
      onOpenWatchTask={watchTaskLink ? fn() : undefined}
    />
  );
}

const meta = {
  title: "Code/Workspace workflow",
  component: WorkflowState,
  args: { snapshot: openPrGit },
  decorators: [
    (Story) => (
      <div className="flex justify-end pt-8 pr-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof WorkflowState>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  args: { snapshot: null },
};

export const Uncommitted: Story = {
  args: { snapshot: dirtyGit },
};

export const ReadyForPullRequest: Story = {
  args: { snapshot: readyForPrGit },
};

export const PullRequestOpen: Story = {};

/** A durable watch task is polling; the segment links into the fork. */
export const Watching: Story = {
  args: { snapshot: watchingPrGit, watchTaskLink: true },
};

/** The watch task is running a fix turn against failing checks. */
export const WatchFixing: Story = {
  args: { snapshot: fixingPrGit },
};

/** The watch task parked on something only the user can do. */
export const WatchBlocked: Story = {
  args: { snapshot: blockedWatchPrGit },
};

export const StoppingWatch: Story = {
  args: { snapshot: watchingPrGit, busy: "stop_watch" },
};
