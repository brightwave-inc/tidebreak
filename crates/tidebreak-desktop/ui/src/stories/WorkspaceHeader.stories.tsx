import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";

import type {
  Attention,
  CodeSessionSnapshot,
  CodeWorkspacePrSnapshot,
  PermissionMode,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import { AttentionBadge } from "@/code/AttentionBadge";
import { SessionLifecycleIndicator } from "@/code/SessionLifecycleIndicator";
import { SessionPermissionIndicator } from "@/code/SessionPermissionIndicator";
import { setEditorPreference } from "@/code/editorPreference";
import {
  WorkspaceOverflowMenu,
  workspaceHeaderCommands,
} from "@/code/workspaceActions";
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
  queuedPrGit,
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
  permissionMode = "allow",
  canOpenWorktree = false,
  canOpenInEditor = false,
}: {
  snapshot: CodeWorkspacePrSnapshot | null;
  loading?: boolean;
  initialReviewOpen?: boolean;
  activityLabel?: string;
  attention?: Attention;
  lifecycle?: CodeSessionSnapshot["lifecycle"];
  pendingApprovals?: number;
  unrecognizedEventCount?: number;
  permissionMode?: PermissionMode;
  /** This window hosts the worktree, so the folder action is truthful. */
  canOpenWorktree?: boolean;
  /** An editor on this machine can open the worktree's files. */
  canOpenInEditor?: boolean;
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
            <span className="text-border" aria-hidden>
              ·
            </span>
            <SessionPermissionIndicator mode={permissionMode} />
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
        <WorkspaceOverflowMenu
          commands={workspaceHeaderCommands({
            archived: false,
            hasSession: true,
            attentionPinned: false,
            canFork: true,
            canOpenWorktree,
            canOpenInEditor,
            quickActions: [],
          })}
          context={{
            repoName: "tidebreak",
            worktreePath: "/Users/sam/tidebreak/worktrees/ui-pane-redesign",
          }}
          onCommand={fn()}
        />
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

/** The workflow mark matches GitHub's orange merge-queue state. */
export const MergeQueued: Story = {
  args: { snapshot: queuedPrGit },
};

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

/**
 * The posture the session runs under, next to its lifecycle. Create picks the
 * most autonomous mode the engine honors, so the header is where a reader
 * learns which one they got.
 */
export const AsksBeforeEachTool: Story = {
  args: { snapshot: openPrGit, permissionMode: "ask" },
};

export const RunsOnItsOwn: Story = {
  args: { snapshot: openPrGit, permissionMode: "auto" },
};

export const PlansOnly: Story = {
  args: {
    snapshot: openPrGit,
    permissionMode: "plan",
    lifecycle: "idle",
    activityLabel: "Idle",
  },
};

/**
 * The permission chip sits in the same muted row as lifecycle. At this width
 * the title and status wrap; the chip truncates instead of overflowing.
 */
export const NarrowAskChip: Story = {
  args: { snapshot: openPrGit, permissionMode: "ask" },
  decorators: [
    (Story) => (
      <div className="mx-auto w-[360px] overflow-hidden rounded-xl border border-border-subtle bg-background">
        <Story />
        <div className="h-24 bg-background" />
      </div>
    ),
  ],
};

export const Loading: Story = {
  args: { snapshot: null, loading: true },
};

/** Session overflow: copy debug JSON, then Uneff me. */
export const OverflowActions: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      canvas.getByRole("button", { name: "Workspace actions" }),
    );
    const menu = within(document.body);
    await expect(
      await menu.findByRole("menuitem", { name: "Copy debug JSON" }),
    ).toBeVisible();
    await expect(
      await menu.findByRole("menuitem", { name: "Uneff me" }),
    ).toBeVisible();
  },
};

export const ReviewClosed: Story = {
  args: { snapshot: openPrGit, initialReviewOpen: false },
};

/**
 * Local overflow: the folder action, then the editor by name. The two sit
 * together because they answer the same question — get me out of this window
 * and into these files.
 */
export const OpenInEditorOverflow: Story = {
  args: { canOpenWorktree: true, canOpenInEditor: true },
  beforeEach: () => {
    setEditorPreference({ editor: "cursor", customProgram: "" });
    return () => setEditorPreference({ editor: "vscode", customProgram: "" });
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      canvas.getByRole("button", { name: "Workspace actions" }),
    );
    const menu = within(document.body);
    await waitFor(async () =>
      expect(
        await menu.findByRole("menuitem", { name: "Open worktree folder" }),
      ).toBeVisible(),
    );
    await waitFor(async () =>
      expect(
        await menu.findByRole("menuitem", { name: "Open in Cursor" }),
      ).toBeVisible(),
    );
  },
};
