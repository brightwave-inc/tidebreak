import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { HttpError, type CodeWorkspacePrSnapshot } from "@/api";
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
  mutationError: string | null = null,
  setMutationError: (error: string | null) => void = () => {},
): CodeWorkspacePrResource {
  return {
    data: snapshot,
    error: null,
    refreshing: false,
    refresh: async () => {},
    adopt: () => {},
    busy,
    mutationError,
    setMutationError,
    refreshFromHost: async () => undefined,
    runMutation: async (_mutation, operation) => operation(),
  };
}

function WorkflowState({
  snapshot,
  busy = null,
  watchTaskLink = false,
  checkLogsHang = false,
  mergeConflict = false,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  busy?: CodeWorkspacePrMutation | null;
  /** Offer the "Watching in …" link the workspace page wires up. */
  watchTaskLink?: boolean;
  /** Leave the check-log download in flight, to hold the reading state. */
  checkLogsHang?: boolean;
  /** Reject merge after confirmation because the pull request head moved. */
  mergeConflict?: boolean;
}) {
  const [mutationError, setMutationError] = useState<string | null>(null);
  return (
    <WorkspaceWorkflowControl
      client={{
        pushCodeWorkspace: fn(),
        createCodePullRequest: fn(),
        markCodePrReady: fn(),
        mergeCodePr: mergeConflict
          ? fn(async () => {
              throw new HttpError(
                409,
                "409: pull request head changed from 8a1f2240 to d94e0301",
                "pr_head_changed",
              );
            })
          : fn(),
        startCodeWatch: fn(),
        stopCodeWatch: fn(),
        writeCodeCheckLogs: checkLogsHang
          ? () => new Promise(() => {})
          : fn(async () => ({ logs: [], errors: [] })),
      }}
      workspaceId="ws-1"
      branchName="tidebreak/scoped-ui-workshop"
      baseRef="main"
      resource={resourceFor(snapshot, busy, mutationError, setMutationError)}
      onOpenSourceControl={fn()}
      onOpenPr={fn()}
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

/** The server rechecked the confirmed merge and found a replacement head. */
export const MergePreconditionChanged: Story = {
  args: {
    snapshot: {
      ...openPrGit,
      ahead: 0,
      pr: {
        ...openPrGit.pr!,
        head_branch: "tidebreak/scoped-ui-workshop",
        base_branch: "main",
        head_sha: "8a1f2240e66d32a8",
        mergeable: "mergeable",
        merge_state_status: "clean",
        checks_summary: "9 passing, 0 pending, 0 failing",
        checks: [{ name: "required checks", bucket: "pass" }],
      },
    },
    mergeConflict: true,
  },
};

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
};
