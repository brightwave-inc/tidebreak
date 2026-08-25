import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { MoreHorizontal } from "lucide-react";
import { fn } from "storybook/test";

import type {
  Attention,
  CodeSessionSnapshot,
  CodeWorkspacePrSnapshot,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { AttentionBadge } from "@/code/AttentionBadge";
import { SessionLifecycleIndicator } from "@/code/SessionLifecycleIndicator";
import { WorkspaceHeader } from "@/code/WorkspaceHeader";
import { WorkspaceWorkflowControl } from "@/code/WorkspaceWorkflowControl";
import type {
  CodeWorkspacePrMutation,
  CodeWorkspacePrResource,
} from "@/code/useCodeWorkspacePr";
import {
  attentionDoneUnreviewed,
  attentionFenced,
  attentionNeedsYou,
  attentionStalled,
  attentionWorking,
  dirtyGit,
  openPrGit,
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

function HeaderState({
  snapshot,
  loading = false,
  initialReviewOpen = true,
  activityLabel = "Agent working",
  attention = attentionWorking,
  lifecycle = "running",
  pendingApprovals = 0,
  unrecognizedEventCount = 0,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  loading?: boolean;
  initialReviewOpen?: boolean;
  activityLabel?: string;
  attention?: Attention;
  lifecycle?: CodeSessionSnapshot["lifecycle"];
  pendingApprovals?: number;
  unrecognizedEventCount?: number;
}) {
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [reviewOpen, setReviewOpen] = useState(initialReviewOpen);

  return (
    <WorkspaceHeader
      title={loading ? undefined : "Reconsider the workspace pane system"}
      repoName={loading ? undefined : "tidebreak"}
      branchName={loading ? undefined : "thet/ui-pane-redesign"}
      worktreePath="/Users/sam/tidebreak/worktrees/ui-pane-redesign"
      loading={loading}
      workflow={
        loading ? undefined : (
          <WorkspaceWorkflowControl
            client={{
              pushCodeWorkspace: fn(),
              createCodePullRequest: fn(),
              markCodePrReady: fn(),
              mergeCodePr: fn(),
              startCodeWatch: fn(),
              stopCodeWatch: fn(),
              writeCodeCheckLogs: fn(async () => ({ logs: [], errors: [] })),
            }}
            workspaceId="ws-story"
            branchName="thet/ui-pane-redesign"
            baseRef="main"
            resource={resourceFor(snapshot)}
            onOpenSourceControl={fn()}
            onOpenWatchTask={snapshot?.watch ? fn() : undefined}
          />
        )
      }
      sessionStatus={
        loading ? undefined : (
          <>
            {attention?.state.type !== "working" && (
              <AttentionBadge attention={attention} compact />
            )}
            {pendingApprovals > 0 && (
              <Badge variant="warning" size="sm">
                {pendingApprovals}{" "}
                {pendingApprovals === 1 ? "approval" : "approvals"}
              </Badge>
            )}
            <SessionLifecycleIndicator
              lifecycle={lifecycle}
              harness="codex"
              version="0.84.0"
              unrecognizedEventCount={unrecognizedEventCount}
              runningLabel={lifecycle === "running" ? activityLabel : undefined}
            />
          </>
        )
      }
      terminalOpen={terminalOpen}
      reviewOpen={reviewOpen}
      terminalShortcut="⌘J"
      reviewShortcut="⌘⇧R"
      onToggleTerminal={() => setTerminalOpen((open) => !open)}
      onToggleReview={() => setReviewOpen((open) => !open)}
      overflowAction={
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="rounded-lg"
          aria-label="Workspace actions"
        >
          <MoreHorizontal />
        </Button>
      }
    />
  );
}

const meta = {
  title: "Code/Workspace header",
  component: HeaderState,
  args: { snapshot: openPrGit },
  decorators: [
    (Story) => (
      <div className="mx-auto w-full max-w-6xl overflow-hidden rounded-xl border border-border-subtle bg-background">
        <Story />
        <div className="h-52 bg-background" />
      </div>
    ),
  ],
} satisfies Meta<typeof HeaderState>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PullRequestOpen: Story = {};

export const LocalChanges: Story = {
  args: { snapshot: dirtyGit },
};

export const WatchTaskActive: Story = {
  args: { snapshot: watchingPrGit },
};

export const ShellRunning: Story = {
  args: { snapshot: openPrGit, activityLabel: "Shell running" },
};

export const Monitoring: Story = {
  args: { snapshot: openPrGit, activityLabel: "Monitoring" },
};

export const SubagentsWorking: Story = {
  args: { snapshot: openPrGit, activityLabel: "2 subagents working" },
};

export const NeedsYou: Story = {
  args: { snapshot: openPrGit, attention: attentionNeedsYou },
};

export const ApprovalPending: Story = {
  args: { snapshot: openPrGit, pendingApprovals: 1 },
};

export const Stalled: Story = {
  args: {
    snapshot: openPrGit,
    attention: attentionStalled,
    lifecycle: "idle",
  },
};

export const Fenced: Story = {
  args: {
    snapshot: openPrGit,
    attention: attentionFenced,
    lifecycle: "fenced",
  },
};

export const DoneUnreviewed: Story = {
  args: {
    snapshot: openPrGit,
    attention: attentionDoneUnreviewed,
    lifecycle: "ended",
  },
};

export const UnrecognizedEngineEvents: Story = {
  args: {
    snapshot: openPrGit,
    activityLabel: "Shell running",
    unrecognizedEventCount: 3,
  },
};

export const Loading: Story = {
  args: { snapshot: null, loading: true },
};

export const ReviewClosed: Story = {
  args: { snapshot: openPrGit, initialReviewOpen: false },
};
