import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { MoreHorizontal } from "lucide-react";
import { fn } from "storybook/test";

import type { CodeWorkspacePrSnapshot } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { WorkspaceHeader } from "@/code/WorkspaceHeader";
import { WorkspaceWorkflowControl } from "@/code/WorkspaceWorkflowControl";
import type {
  CodeWorkspacePrMutation,
  CodeWorkspacePrResource,
} from "@/code/useCodeWorkspacePr";
import { dirtyGit, openPrGit, watchingPrGit } from "./fixtures";

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
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  loading?: boolean;
  initialReviewOpen?: boolean;
  activityLabel?: string;
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
          <Badge variant="outline" size="sm">
            {activityLabel}
          </Badge>
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

export const Loading: Story = {
  args: { snapshot: null, loading: true },
};

export const ReviewClosed: Story = {
  args: { snapshot: openPrGit, initialReviewOpen: false },
};
