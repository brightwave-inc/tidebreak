import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";

import type { CodeWorkspacePrSnapshot } from "@/api";
import { WorkspaceWorkflowControl } from "@/code/WorkspaceWorkflowControl";
import type {
  CodeWorkspacePrMutation,
  CodeWorkspacePrResource,
} from "@/code/useCodeWorkspacePr";
import {
  blockedWatchPrGit,
  dirtyGit,
  failingChecksPrGit,
  fixingPrGit,
  needsApprovalPrGit,
  openPrGit,
  queuedPrGit,
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
  checkLogsHang = false,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  busy?: CodeWorkspacePrMutation | null;
  /** Offer the "Watching in …" link the workspace page wires up. */
  watchTaskLink?: boolean;
  /** Leave the check-log download in flight, to hold the reading state. */
  checkLogsHang?: boolean;
}) {
  return (
    <WorkspaceWorkflowControl
      client={{
        pushCodeWorkspace: fn(),
        createCodePullRequest: fn(),
        markCodePrReady: fn(),
        mergeCodePr: fn(),
        startCodeWatch: fn(),
        stopCodeWatch: fn(),
        writeCodeCheckLogs: checkLogsHang
          ? () => new Promise(() => {})
          : fn(async () => ({ logs: [], errors: [] })),
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

/** GitHub's merge queue uses the shared orange status treatment. */
export const MergeQueued: Story = {
  args: { snapshot: queuedPrGit },
};

/** Checks are green; GitHub still wants a review approval (decision 66). */
export const NeedsApproval: Story = {
  args: { snapshot: needsApprovalPrGit },
};

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

/** Failing checks with nobody watching: the button offers Fix errors. */
export const FailingChecks: Story = {
  args: { snapshot: failingChecksPrGit },
};

/**
 * Pressed, and the failing jobs' logs are still downloading.
 *
 * The download is a host read the reader waits on before the turn starts, so
 * the button says so rather than sitting on the action label doing nothing.
 */
export const ReadingCheckLogs: Story = {
  args: { snapshot: failingChecksPrGit, checkLogsHang: true },
  play: async ({ canvasElement }) => {
    const button = within(canvasElement).getByRole("button", {
      name: /fix ci/i,
    });
    await userEvent.click(button);
    await within(canvasElement).findByText("Reading logs…");
  },
};
