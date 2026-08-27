import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, waitFor, within } from "storybook/test";
import { toast } from "sonner";

import { setEditorPreference } from "@/code/editorPreference";
import {
  workspaceCommands,
  worktreeOpenFailureNotice,
} from "@/code/workspaceActions";
import { WorkspaceCard } from "@/code/WorkspaceCard";
import { Toaster } from "@/components/ui/sonner";
import {
  archivedWorkspace,
  releasedWorkspace,
  closedPrDigest,
  codeSession,
  codeWorkspace,
  doneDigest,
  draftPrDigest,
  grokIdleSession,
  idleCompleteDigest,
  idleDigest,
  idleSession,
  mergedPrDigest,
  monitorDigest,
  needsYouDigest,
  openPrDigest,
  queuedPrDigest,
  runningDigest,
  shellDigest,
  stalledDigest,
  subagentsDigest,
  watchDigest,
} from "./fixtures";

/**
 * The rail's workspace row. The row keeps the triage state visible. Hovering
 * opens the full branch, pull-request checks, review state, and actions.
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

/** A create appears in the rail while the worktree and first session start. */
export const Creating: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      id: "optimistic-workspace:storybook",
      title: "New workspace",
      branch_name: "",
      worktree_path: "",
      status: "creating",
      created_at: "2026-08-24T12:00:00.000Z",
    },
    visibleMeta: { repoChip: true, branch: false },
    commands: [],
  },
};

/**
 * The setup script failed. The checkout and branch survived, so the row keeps
 * its commands and offers the retry — the critical line is the only thing that
 * says the script never finished.
 */
export const SetupFailed: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      id: "ws-setup-failed",
      title: "Rework the credential exchange",
      status: "setup_failed",
    },
    visibleMeta: { repoChip: true, branch: true },
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      setupFailed: true,
    }),
  },
};

/** A green pull request with the hover detail held open for visual review. */
export const HoverPullRequestReady: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      pr: {
        ...openPrDigest,
        checks_summary: "9 passing",
        review_decision: "approved",
        mergeable: "mergeable",
        merge_state_status: "clean",
        head_branch: codeWorkspace.branch_name,
        base_branch: "main",
      },
    },
    digest: {
      ...runningDigest,
      pr_state: {
        ...openPrDigest,
        checks_summary: "9 passing",
        review_decision: "approved",
        mergeable: "mergeable",
        merge_state_status: "clean",
        head_branch: codeWorkspace.branch_name,
        base_branch: "main",
      },
    },
    session: codeSession,
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
    onWorkflowAction: fn(),
  },
};

/** Review and check blockers stay readable without opening the workspace. */
export const HoverPullRequestBlocked: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      pr: {
        ...openPrDigest,
        checks_summary: "6 passing, 2 failing",
        review_decision: "changes_requested",
        mergeable: "mergeable",
        merge_state_status: "blocked",
        head_branch: codeWorkspace.branch_name,
        base_branch: "main",
      },
    },
    digest: {
      ...doneDigest,
      pr_state: {
        ...openPrDigest,
        checks_summary: "6 passing, 2 failing",
        review_decision: "changes_requested",
        mergeable: "mergeable",
        merge_state_status: "blocked",
        head_branch: codeWorkspace.branch_name,
        base_branch: "main",
      },
    },
    session: codeSession,
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
    onWorkflowAction: fn(),
  },
};

/** A structured question gets the same hover surface without PR filler. */
export const HoverNeedsYou: Story = {
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

/** Compact rail rows still reveal the full workspace and PR detail. */
export const HoverCompact: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      pr: {
        ...openPrDigest,
        checks_summary: "7 passing, 1 pending",
        review_decision: "review_required",
        mergeable: "unknown",
        merge_state_status: "unknown",
        head_branch: codeWorkspace.branch_name,
        base_branch: "main",
      },
    },
    density: "compact",
    commands: workspaceCommands({ hasPr: true, archived: false }),
    detailDefaultOpen: true,
  },
};

/** The rail keeps the PR glyph and live activity without repeating PR detail. */
export const PullRequestInRail: Story = {
  args: {
    workspace: { ...codeWorkspace, pr: openPrDigest },
    digest: { ...runningDigest, pr_state: openPrDigest },
    session: codeSession,
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    onWorkflowAction: fn(),
  },
};

/** Queue membership replaces the open chip in the hover detail. */
export const MergeQueued: Story = {
  args: {
    workspace: { ...codeWorkspace, pr: queuedPrDigest },
    digest: { ...doneDigest, pr_state: queuedPrDigest },
    session: codeSession,
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
    onWorkflowAction: fn(),
  },
};

/**
 * A branch based on a sibling workspace's branch (decision 62): the stack
 * relationship nests as a child row that opens the parent workspace.
 */
export const StackedOnSibling: Story = {
  args: {
    workspace: { ...codeWorkspace, base_ref: "origin/tidebreak/base-work" },
    digest: runningDigest,
    session: codeSession,
    stackParent: { id: "ws-parent", title: "Extract the fact store" },
    onOpenStackParent: fn(),
  },
};

/**
 * A workspace that worked on several pull requests (decision 62): the chip
 * keeps its primary pull request and gains the attributed count.
 */
export const SeveralPullRequests: Story = {
  args: {
    workspace: { ...codeWorkspace, pr: openPrDigest },
    digest: { ...runningDigest, pr_state: openPrDigest, pr_count: 3 },
    session: codeSession,
    commands: workspaceCommands({
      hasPr: true,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
    onWorkflowAction: fn(),
  },
};

/** The hover detail offers Merge for an approved, green pull request. */
export const ReadyToMerge: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      pr: {
        ...openPrDigest,
        review_decision: "approved",
        mergeable: "mergeable",
        merge_state_status: "clean",
        checks_summary: "9 passing",
      },
    },
    commands: workspaceCommands({ hasPr: true, archived: false }),
    detailDefaultOpen: true,
    onWorkflowAction: fn(),
  },
};

/** The hover detail leads a conflicting PR with Resolve conflicts. */
export const Conflicts: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      pr: { ...openPrDigest, mergeable: "conflicting" },
    },
    commands: workspaceCommands({ hasPr: true, archived: false }),
    detailDefaultOpen: true,
    onWorkflowAction: fn(),
  },
};

/** The row when the session is waiting on the reader. */
export const NeedsYouInline: Story = {
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

/** An older resting digest cannot make the rail look live beside an Idle panel. */
export const HoverIdleSession: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      title: "Idle workspace",
      branch_name: "tidebreak/idle-workspace",
    },
    digest: {
      ...idleDigest,
      title: "Idle workspace",
    },
    session: idleSession,
    active: true,
    visibleMeta: { repoChip: true, branch: true },
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
    detailDefaultOpen: true,
  },
};

/**
 * A parked agent still owns the row. The quiet Done mark and the harness
 * keep it from looking like an empty workspace after the turn completes.
 */
export const ParkedComplete: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      title: "Parked exploration",
      branch_name: "tidebreak/parked-exploration",
    },
    digest: {
      ...idleCompleteDigest,
      title: "Parked exploration",
    },
    session: grokIdleSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

/** Parked without a recap still names the agent and the completed turn. */
export const ParkedIdle: Story = {
  args: {
    workspace: {
      ...codeWorkspace,
      title: "Product direction hypotheses",
    },
    digest: {
      ...idleDigest,
      title: "Product direction hypotheses",
    },
    session: idleSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

/** Harness mark, live activity, and turn count on the detailed status line. */
export const RunningSession: Story = {
  args: {
    digest: runningDigest,
    session: codeSession,
    visibleMeta: { repoChip: true, branch: true },
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

/** The workspace summary follows the running Claude session, not stopped Codex. */
export const RunningClaudeAfterStoppedCodex: Story = {
  args: {
    digest: {
      ...runningDigest,
      session: "sess-claude",
      harness_kind: "claude_code",
      turn_count: 1,
    },
    session: {
      ...codeSession,
      id: "sess-codex",
      harness_kind: "codex",
      lifecycle: "ended",
    },
    active: true,
    visibleMeta: { repoChip: true, branch: true },
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

/** A live turn waiting on a command says so instead of implying generation. */
export const ShellRunning: Story = {
  args: {
    digest: shellDigest,
    session: codeSession,
    terminalOpen: true,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
  },
};

/** A passive output/watch tool remains active without reading as an agent. */
export const Monitoring: Story = {
  args: {
    digest: monitorDigest,
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

/**
 * Harness subagents riding the digest as child rows (ADR 0052): running,
 * done, and failed. Clicking one opens the workspace; the filtered
 * sub-transcript view is a later slice.
 */
export const WithSubagents: Story = {
  args: {
    digest: subagentsDigest,
    session: codeSession,
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
    }),
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

/** On the shelf: a dimmed row with Restore in the hover detail. */
export const Archived: Story = {
  args: {
    workspace: archivedWorkspace,
    commands: workspaceCommands({ hasPr: false, archived: true }),
    detailDefaultOpen: true,
  },
};

/**
 * Released reads as put-away, like Archived: the rail must not show a
 * workspace whose branch is gone as live work.
 */
export const Released: Story = {
  args: {
    workspace: releasedWorkspace,
    commands: workspaceCommands({ hasPr: false, archived: true }),
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

/**
 * The status ramp on one screen, which is the only way to catch a tone drawn
 * at the wrong strength. Read down the glyph rail: working moves, needs-you is
 * the one red, stalled is amber, done is quiet, parked still shows the agent,
 * and merged is purple rather than a second shade of green. Check this in both
 * themes.
 */
export const StatusTones: Story = {
  render: (args) => (
    <div className="flex flex-col gap-0.5">
      {(
        [
          ["Working", runningDigest, codeSession, undefined],
          ["Needs you", needsYouDigest, codeSession, undefined],
          ["Stalled", stalledDigest, codeSession, undefined],
          ["Done, unreviewed", doneDigest, idleSession, undefined],
          ["Parked, complete", idleCompleteDigest, grokIdleSession, undefined],
          ["PR merged", undefined, undefined, mergedPrDigest],
          ["No session", undefined, undefined, undefined],
        ] as const
      ).map(([label, digest, session, pr]) => (
        <WorkspaceCard
          {...args}
          key={label}
          workspace={{
            ...codeWorkspace,
            id: `ws-${label}`,
            title: label,
            pr,
          }}
          digest={digest && { ...digest, title: label }}
          session={session}
          commands={workspaceCommands({
            hasPr: Boolean(pr),
            archived: false,
            hasSession: Boolean(digest),
          })}
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
        digest={{ ...idleCompleteDigest, title: "Parked exploration" }}
        session={grokIdleSession}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
      />
    </div>
  ),
};

/** A local workspace leads with the file manager action in its hover detail. */
export const LocalWorktreeAction: Story = {
  args: {
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      canOpenWorktree: true,
    }),
    detailDefaultOpen: true,
  },
};

/** A remote or unsupported client keeps the path-copy fallback in the same place. */
export const RemoteWorktreeFallback: Story = {
  args: {
    commands: workspaceCommands({
      hasPr: false,
      archived: false,
      canOpenWorktree: false,
    }),
    detailDefaultOpen: true,
  },
};

/**
 * The card's own menu, held open: the editor action sits under the worktree
 * folder and names the editor the reader picked, so the row says what it will
 * do before it is chosen.
 */
export const OpenInEditorMenu: Story = {
  beforeEach: () => {
    setEditorPreference({ editor: "zed", customProgram: "" });
    return () => setEditorPreference({ editor: "vscode", customProgram: "" });
  },
  render: (args) => (
    <WorkspaceCard
      {...args}
      commands={workspaceCommands({
        hasPr: false,
        archived: false,
        hasSession: true,
        canOpenWorktree: true,
        canOpenInEditor: true,
      })}
    />
  ),
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.pointer({
      keys: "[MouseRight]",
      target: await body.findByText(codeWorkspace.title),
    });
    await waitFor(() =>
      expect(
        body.getByRole("menuitem", { name: "Open worktree folder" }),
      ).toBeVisible(),
    );
    await expect(
      body.getByRole("menuitem", { name: "Open in Zed" }),
    ).toBeVisible();
  },
};

/** A window attached to another machine gets no editor row to be let down by. */
export const RemoteHasNoEditorAction: Story = {
  render: (args) => (
    <WorkspaceCard
      {...args}
      commands={workspaceCommands({
        hasPr: false,
        archived: false,
        hasSession: true,
        canOpenWorktree: false,
        canOpenInEditor: false,
      })}
    />
  ),
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.pointer({
      keys: "[MouseRight]",
      target: await body.findByText(codeWorkspace.title),
    });
    await waitFor(() =>
      expect(
        body.getByRole("menuitem", { name: "Copy worktree path" }),
      ).toBeVisible(),
    );
    await expect(
      body.queryByRole("menuitem", { name: /^Open in / }),
    ).toBeNull();
  },
};

function RecoverableWorktreeFailure() {
  const notice = worktreeOpenFailureNotice({
    reason: "code_worktree_path_not_found",
    detail: "/private/native/detail",
  });
  return (
    <>
      <WorkspaceCard
        workspace={codeWorkspace}
        digest={undefined}
        session={undefined}
        repoName="tidebreak"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          canOpenWorktree: true,
        })}
        detailDefaultOpen
        onOpen={fn()}
        onCommand={(command) => {
          if (command !== "open-worktree") return;
          toast.error(notice.title, {
            description: notice.description,
            action: { label: notice.actionLabel, onClick: fn() },
          });
        }}
      />
      <Toaster richColors duration={Number.POSITIVE_INFINITY} />
    </>
  );
}

/** A failed native open leaves a clear recovery action instead of disappearing. */
export const WorktreeOpenFailure: Story = {
  render: () => <RecoverableWorktreeFailure />,
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await body.findByRole("button", { name: "Open folder" }),
    );
    await waitFor(() =>
      expect(body.getByText("Could not open worktree folder")).toBeVisible(),
    );
    await waitFor(() =>
      expect(body.getByRole("button", { name: "Copy path" })).toBeVisible(),
    );
  },
};
