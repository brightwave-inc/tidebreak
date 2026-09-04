// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { workspaceBulkCommands, workspaceCommands } from "./workspaceActions";
import { WorkspaceCard } from "./WorkspaceCard";
import type {
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import type { OptimisticCodeWorkspaceSnapshot } from "./CodeCatalogStore";

const workspace: CodeWorkspaceSnapshot = {
  id: "ws-1",
  repo_id: "repo-1",
  title: "Fix login",
  worktree_path: "/tmp/app/.worktrees/fix-login",
  branch_name: "tidebreak/fix-login",
  base_ref: "main",
  status: "active",
  created_at: "2026-08-15T00:00:00.000Z",
};

const pr: PullRequestDigest = {
  number: 184,
  url: "https://github.com/example/app/pull/184",
  state: "open",
  checks_summary: "8 passing, 1 pending",
};

afterEach(cleanup);

function renderCard(overrides?: {
  workspace?: Partial<OptimisticCodeWorkspaceSnapshot>;
  pr?: PullRequestDigest;
  digest?: CodeSessionDigest;
  session?: CodeSessionSnapshot;
  density?: "compact" | "detailed";
  visibleMeta?: { repoChip: boolean; branch: boolean };
  detailDefaultOpen?: boolean;
  canOpenWorktree?: boolean;
  active?: boolean;
  selected?: boolean;
  commands?: ReturnType<typeof workspaceCommands>;
  contextMenuLabel?: string;
}) {
  const onOpen = vi.fn();
  const onCommand = vi.fn();
  const onSelectPointer = vi.fn();
  const merged = {
    ...workspace,
    ...overrides?.workspace,
    pr: overrides?.pr,
  };
  render(
    <WorkspaceCard
      workspace={merged}
      digest={overrides?.digest}
      session={overrides?.session}
      repoName="app"
      active={overrides?.active ?? false}
      selected={overrides?.selected}
      terminalOpen={false}
      density={overrides?.density ?? "detailed"}
      visibleMeta={overrides?.visibleMeta ?? { repoChip: true, branch: false }}
      commands={
        overrides?.commands ??
        workspaceCommands({
          hasPr: Boolean(overrides?.pr),
          archived: merged.status === "archived",
          canOpenWorktree: overrides?.canOpenWorktree,
        })
      }
      contextMenuLabel={overrides?.contextMenuLabel}
      detailDefaultOpen={overrides?.detailDefaultOpen}
      onOpen={onOpen}
      onSelectPointer={onSelectPointer}
      onCommand={onCommand}
    />,
  );
  return { onOpen, onCommand, onSelectPointer };
}

describe("WorkspaceCard", () => {
  it("opens the workspace from the row", async () => {
    const user = userEvent.setup();
    const { onOpen } = renderCard();

    await user.click(
      screen.getByRole("button", {
        name: "Fix login · app · tidebreak/fix-login",
      }),
    );
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("shows a non-interactive creating state", async () => {
    const user = userEvent.setup();
    const { onOpen } = renderCard({
      workspace: {
        status: "creating",
        branch_name: "",
        worktree_path: "",
      },
    });

    const row = screen.getByRole("button", {
      name: "Fix login · Creating branch and folder · app",
    });
    const progress = screen.getByRole("status", {
      name: "Creating branch and folder",
    });
    expect(row).toBeDisabled();
    expect(progress).toHaveTextContent("Creating branch and folder");
    expect(progress.querySelector(".live-label-shimmer")).not.toBeNull();
    expect(
      progress.querySelector(".workspace-creation-progress"),
    ).not.toBeNull();
    await user.click(row);
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("keeps the creation transition visible in compact mode", () => {
    renderCard({
      density: "compact",
      workspace: {
        status: "creating",
        branch_name: "",
        worktree_path: "",
      },
    });

    expect(
      screen.getByRole("status", { name: "Creating branch and folder" }),
    ).toBeInTheDocument();
  });

  it("shows naming before the derived title lands", () => {
    renderCard({
      workspace: {
        status: "creating",
        branch_name: "",
        worktree_path: "",
        optimistic_creation_phase: "naming",
      },
    });

    expect(
      screen.getByRole("button", {
        name: "Fix login · Naming workspace · app",
      }),
    ).toBeDisabled();
    expect(
      screen.getByRole("status", { name: "Naming workspace" }),
    ).toHaveTextContent("Naming workspace");
  });

  it("keeps one menu: right-click carries every command", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({ pr });

    fireEvent.contextMenu(screen.getByRole("button", { name: /^Fix login/ }));
    const menu = await screen.findByRole("menu");
    expect(menu).toHaveTextContent("Open pull request");
    expect(menu).toHaveTextContent("Archive");
    await user.click(screen.getByRole("menuitem", { name: "Toggle terminal" }));
    expect(onCommand).toHaveBeenCalledWith("toggle-terminal");
  });

  it("opens a local worktree from the hover detail and context menu", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({
      canOpenWorktree: true,
      detailDefaultOpen: true,
    });

    await user.click(screen.getByRole("button", { name: "Open folder" }));
    expect(onCommand).toHaveBeenCalledWith("open-worktree");

    fireEvent.contextMenu(screen.getByRole("button", { name: /^Fix login/ }));
    expect(
      await screen.findByRole("menuitem", { name: "Open worktree folder" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "Copy worktree path" }),
    ).not.toBeInTheDocument();
  });

  it("keeps Copy path as the unsupported or remote fallback", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({ detailDefaultOpen: true });

    await user.click(screen.getByRole("button", { name: "Copy path" }));
    expect(onCommand).toHaveBeenCalledWith("copy-worktree");
  });

  it("announces a queued pull request and paints it info blue", () => {
    renderCard({
      pr: { ...pr, in_merge_queue: true },
      detailDefaultOpen: true,
    });

    expect(
      screen.getByRole("button", { name: /Pull request #184 In merge queue/ }),
    ).toBeInTheDocument();
    expect(screen.getAllByText("In merge queue")[0]).toHaveClass(
      "bg-info-background",
    );
  });
  it("keeps only the PR glyph in the row and puts its action in the hover detail", async () => {
    const user = userEvent.setup();
    const { onOpen, onCommand } = renderCard({
      pr,
      detailDefaultOpen: true,
    });
    const row = screen.getByRole("button", { name: /^Fix login/ });

    expect(row.querySelector('[data-pr-state="open"]')).toBeInTheDocument();
    expect(row).not.toHaveTextContent("#184");

    await user.click(
      screen.getByRole("button", { name: "Open pull request #184" }),
    );
    expect(onCommand).toHaveBeenCalledWith("open-pr");
    // PR actions are siblings of the conversation row, never nested controls.
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("restores the hover detail with branch, checks, review, and actions", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({
      pr: {
        ...pr,
        review_decision: "review_required",
        mergeable: "mergeable",
        merge_state_status: "blocked",
        base_branch: "main",
      },
      detailDefaultOpen: true,
    });

    const detail = screen.getByTestId("workspace-hover-card");
    expect(detail).toHaveTextContent("tidebreak/fix-login");
    expect(detail).toHaveTextContent("8 passing · 1 pending");
    expect(detail).toHaveTextContent("Review required");
    expect(detail).toHaveTextContent("into main");

    await user.click(screen.getByRole("button", { name: "Archive" }));
    expect(onCommand).toHaveBeenCalledWith("archive");
  });

  it("speaks the header control's workflow vocabulary from the panel", async () => {
    const user = userEvent.setup();
    const onWorkflowAction = vi.fn();
    render(
      <WorkspaceCard
        workspace={{
          ...workspace,
          pr: { ...pr, mergeable: "conflicting", checks_summary: undefined },
        }}
        digest={undefined}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({ hasPr: true, archived: false })}
        detailDefaultOpen
        onOpen={vi.fn()}
        onCommand={vi.fn()}
        onWorkflowAction={onWorkflowAction}
      />,
    );

    // Same model, same label table as WorkspaceWorkflowControl: a
    // conflicting PR's one obvious action is resolving the conflicts.
    await user.click(screen.getByRole("button", { name: "Resolve conflicts" }));
    expect(onWorkflowAction).toHaveBeenCalledWith("resolve_conflicts");
  });

  it("leads an archived workspace's panel with Restore", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({
      workspace: {
        status: "archived",
        archived_at: "2026-08-18T00:00:00.000Z",
      },
      detailDefaultOpen: true,
    });

    expect(screen.getByText("Archived")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Restore" }));
    expect(onCommand).toHaveBeenCalledWith("restore");

    fireEvent.contextMenu(screen.getByRole("button", { name: /^Fix login/ }));
    const menu = await screen.findByRole("menu");
    expect(menu).toHaveTextContent("Restore workspace");
    expect(menu).not.toHaveTextContent("Toggle terminal");
  });

  it("shows meta per view and keeps the label complete without it", () => {
    renderCard({ visibleMeta: { repoChip: false, branch: false } });

    // Repo and branch stay in the accessible name even when nothing meta is
    // drawn on the row.
    expect(screen.queryByText("app")).not.toBeInTheDocument();
    expect(screen.queryByText("tidebreak/fix-login")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Fix login · app · tidebreak/fix-login",
      }),
    ).toBeInTheDocument();
  });

  it("keeps a workspace without a session looking empty", () => {
    renderCard();

    expect(screen.getByLabelText("Idle")).toBeInTheDocument();
    expect(screen.queryByText("Idle")).not.toBeInTheDocument();
    expect(screen.queryByText("Done")).not.toBeInTheDocument();
    expect(screen.queryByTitle("Claude Code")).not.toBeInTheDocument();
  });

  it("keeps a silent running command out of needs-you", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      harness_kind: "claude_code",
      lifecycle: "running",
      attention: {
        state: { type: "stalled", idle_secs: 1_140 },
        source: "heuristic",
      },
      title: workspace.title,
      turn_count: 3,
      activity: "shell",
      activity_detail: "git commit -m release-notes",
    };
    renderCard({
      digest,
      session: {
        id: "sess-1",
        workspace_id: workspace.id,
        kind: "interactive",
        harness_kind: "claude_code",
        execution_location: "machine",
        permission_mode: "plan",
        fast_mode: false,
        lifecycle: "running",
        attention: digest.attention,
        unrecognized_event_count: 0,
        created_at: "2026-08-15T00:00:00.000Z",
      },
    });

    expect(screen.getByLabelText("Running")).toBeInTheDocument();
    expect(screen.queryByLabelText("Needs you")).not.toBeInTheDocument();
    expect(screen.getByText("git commit -m release-notes")).toBeInTheDocument();
    expect(
      document.querySelector('[data-state-glyph="stalled"]'),
    ).toBeInTheDocument();
  });

  it("does not open on a modifier click so the rail can select", async () => {
    const { onOpen, onSelectPointer } = renderCard();
    const row = screen.getByRole("button", { name: /^Fix login/ });
    fireEvent.click(row, { metaKey: true });
    expect(onOpen).not.toHaveBeenCalled();
    expect(onSelectPointer).toHaveBeenCalledTimes(1);
    fireEvent.click(row);
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("keeps the active shadow when the open workspace is also selected", () => {
    renderCard({ selected: true });
    const selectedOnly = document.querySelector("[data-workspace-card]");
    expect(selectedOnly).toHaveAttribute("data-selected");
    expect(selectedOnly).not.toHaveAttribute("data-active");
    const selectedOnlyClass = selectedOnly?.className ?? "";
    cleanup();

    renderCard({ selected: true, active: true });
    const selectedAndActive = document.querySelector("[data-workspace-card]");
    expect(selectedAndActive).toHaveAttribute("data-selected");
    expect(selectedAndActive).toHaveAttribute("data-active");
    expect(selectedAndActive?.className).not.toBe(selectedOnlyClass);
    expect(selectedAndActive?.className).toContain("shadow-[");
    expect(selectedOnlyClass).not.toContain("shadow-[");
  });

  it("names a bulk menu without the single-card commands", async () => {
    renderCard({
      selected: true,
      contextMenuLabel: "Workspace",
      commands: workspaceBulkCommands(2),
    });
    fireEvent.contextMenu(screen.getByRole("button", { name: /^Fix login/ }));
    const menu = await screen.findByRole("menu");
    expect(menu).toHaveTextContent("Workspace");
    expect(menu).toHaveTextContent("Archive 2 workspaces");
    expect(menu).toHaveTextContent("Force archive 2 workspaces");
    expect(menu).not.toHaveTextContent("Rename");
    expect(menu).not.toHaveTextContent("New session");
  });

  it("paints a ready-to-merge notice as success, not needs-you", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      lifecycle: "idle",
      attention: {
        state: {
          type: "needs_you",
          prompt: "#184 is ready to merge",
          source: "structured",
        },
        source: "structured",
      },
      title: workspace.title,
      turn_count: 4,
      pr_state: pr,
    };
    render(
      <WorkspaceCard
        workspace={{ ...workspace, pr }}
        digest={digest}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({
          hasPr: true,
          archived: false,
          hasSession: true,
        })}
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    const line = screen.getByText("#184 is ready to merge");
    expect(line).toHaveClass("text-success-foreground");
    expect(line).not.toHaveClass("text-critical-foreground");
    expect(screen.queryByLabelText("Needs you")).not.toBeInTheDocument();
    expect(screen.getByLabelText("PR open")).toBeInTheDocument();
    expect(
      document.querySelector('[data-state-glyph="ready_to_merge"]'),
    ).toBeInTheDocument();
  });

  it("does not keep a ready-to-merge notice after the pull request merges", () => {
    const merged: PullRequestDigest = {
      ...pr,
      state: "merged",
      merged: true,
    };
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      lifecycle: "idle",
      attention: {
        state: {
          type: "needs_you",
          prompt: "#184 is ready to merge",
          source: "structured",
        },
        source: "structured",
      },
      title: workspace.title,
      turn_count: 4,
      recap: "Folded the backoff into refresh.",
      pr_state: merged,
    };
    render(
      <WorkspaceCard
        workspace={{ ...workspace, pr: merged }}
        digest={digest}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({
          hasPr: true,
          archived: false,
          hasSession: true,
        })}
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    expect(screen.queryByText(/ready to merge/i)).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Needs you")).not.toBeInTheDocument();
    expect(
      screen.getByText("Folded the backoff into refresh."),
    ).toBeInTheDocument();
    expect(
      document.querySelector('[data-pr-state="merged"]'),
    ).toBeInTheDocument();
  });

  it("keeps a parked agent visible as a completed turn", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      harness_kind: "claude_code",
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 1,
      recap: "Folded the backoff into refresh.",
    };
    const session: CodeSessionSnapshot = {
      id: "sess-1",
      workspace_id: workspace.id,
      kind: "interactive",
      harness_kind: "claude_code",
      execution_location: "machine",
      permission_mode: "ask",
      fast_mode: false,
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-15T00:00:00.000Z",
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={session}
        repoName="app"
        active
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: true }}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    expect(screen.queryByLabelText("Working")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Done")).toBeInTheDocument();
    expect(screen.getByTitle("Claude Code")).toBeInTheDocument();
    expect(
      screen.getByText("Folded the backoff into refresh."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Fix login · Done · app · tidebreak/fix-login",
      }),
    ).toBeInTheDocument();
  });

  it("announces Done when parked attention is already idle", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      harness_kind: "grok",
      lifecycle: "idle",
      attention: { state: { type: "idle" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 4,
      recap: "Folded the backoff into refresh.",
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("Done")).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Fix login · Done · app · tidebreak/fix-login",
      }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Idle/ }),
    ).not.toBeInTheDocument();
  });

  it("does not show live motion when an older idle digest still says Working", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 1,
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={undefined}
        repoName="app"
        active
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: true }}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
        detailDefaultOpen
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    expect(screen.queryByLabelText("Working")).not.toBeInTheDocument();
    expect(screen.getAllByText("Done").length).toBeGreaterThan(0);
    expect(
      screen.getByRole("button", {
        name: "Fix login · Done · app · tidebreak/fix-login",
      }),
    ).toBeInTheDocument();
  });

  it("compact keeps the one-line read and the complete label", () => {
    renderCard({ density: "compact" });

    expect(screen.queryByText("app")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: "Fix login · app · tidebreak/fix-login",
      }),
    ).toBeInTheDocument();
  });

  it.each([
    ["shell", undefined, "Shell running", "working"],
    [
      "shell",
      "cargo test -p tidebreak-server",
      "cargo test -p tidebreak-server",
      "working",
    ],
    ["monitor", undefined, "Monitoring", "monitor"],
    ["monitor", "watching CI on #3040", "watching CI on #3040", "monitor"],
  ] as const)(
    "renders %s activity (%s) as its own workspace state",
    (activity, activity_detail, label, glyph) => {
      const digest: CodeSessionDigest = {
        workspace: workspace.id,
        session: "sess-1",
        kind: "interactive",
        lifecycle: "running",
        attention: { state: { type: "working" }, source: "lifecycle" },
        title: workspace.title,
        turn_count: 3,
        activity,
        activity_detail,
      };
      render(
        <WorkspaceCard
          workspace={workspace}
          digest={digest}
          session={undefined}
          repoName="app"
          active={false}
          terminalOpen={false}
          density="detailed"
          visibleMeta={{ repoChip: true, branch: false }}
          commands={workspaceCommands({ hasPr: false, archived: false })}
          onOpen={vi.fn()}
          onCommand={vi.fn()}
        />,
      );

      expect(screen.getByText(label)).toBeInTheDocument();
      expect(screen.queryByText(/Agent working/)).not.toBeInTheDocument();
      expect(screen.queryByText(/turns?$/)).not.toBeInTheDocument();
      expect(
        document.querySelector(`[data-state-glyph="${glyph}"]`),
      ).toBeInTheDocument();
    },
  );

  it("detailed running cards carry the state glyph and the harness mark", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 1,
      activity: "agent",
    };
    const session: CodeSessionSnapshot = {
      id: "sess-1",
      workspace_id: workspace.id,
      kind: "interactive",
      harness_kind: "claude_code",
      execution_location: "machine",
      permission_mode: "ask",
      fast_mode: false,
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-15T00:00:00.000Z",
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={session}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: true }}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    expect(screen.getByTitle("Claude Code")).toBeInTheDocument();
    expect(screen.getByText("Agent working")).toBeInTheDocument();
  });

  it("shows the running digest's harness instead of a stopped sibling", () => {
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-claude",
      kind: "interactive",
      harness_kind: "claude_code",
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 1,
      activity: "agent",
    };
    const stoppedCodex: CodeSessionSnapshot = {
      id: "sess-codex",
      workspace_id: workspace.id,
      kind: "interactive",
      harness_kind: "codex",
      execution_location: "machine",
      permission_mode: "ask",
      fast_mode: false,
      lifecycle: "ended",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-14T00:00:00.000Z",
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={stoppedCodex}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: true }}
        commands={workspaceCommands({
          hasPr: false,
          archived: false,
          hasSession: true,
        })}
        onOpen={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    expect(screen.getByTitle("Claude Code")).toBeInTheDocument();
    expect(screen.queryByTitle("Codex")).not.toBeInTheDocument();
    expect(screen.getByText("Agent working")).toBeInTheDocument();
  });

  it("renders a watch task as its own clickable child row", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    const onOpenChildSession = vi.fn();
    const child: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-watch",
      kind: "watch",
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 2,
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={undefined}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({ hasPr: false, archived: false })}
        childSessions={[child]}
        onOpen={onOpen}
        onCommand={vi.fn()}
        onOpenChildSession={onOpenChildSession}
      />,
    );

    await user.click(
      screen.getByRole("button", {
        name: "Watch task for Fix login: Watching",
      }),
    );
    expect(onOpenChildSession).toHaveBeenCalledWith("sess-watch");
    // The child row is a sibling of the row button; opening it must not
    // also open the workspace.
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("renders harness subagents as child rows that open their filtered transcript", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    const onOpenSubagent = vi.fn();
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 3,
      subagents: [
        {
          call_id: "toolu_task_1",
          name: "Find the config parser",
          status: "running",
        },
        {
          call_id: "toolu_task_2",
          name: "Run the flaky suite",
          status: "failed",
        },
      ],
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({ hasPr: false, archived: false })}
        onOpen={onOpen}
        onOpenSubagent={onOpenSubagent}
        onCommand={vi.fn()}
      />,
    );

    // Both rows render with their status; a failed one still names itself.
    expect(
      screen.getByRole("button", {
        name: "Subagent for Fix login: Run the flaky suite, Failed",
      }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", {
        name: "Subagent for Fix login: Find the config parser, Running",
      }),
    );
    expect(onOpenSubagent).toHaveBeenCalledWith("toolu_task_1");
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("folds settled subagents behind a summary row until opened", async () => {
    const user = userEvent.setup();
    const digest: CodeSessionDigest = {
      workspace: workspace.id,
      session: "sess-1",
      kind: "interactive",
      lifecycle: "idle",
      attention: { state: { type: "done_unreviewed" }, source: "lifecycle" },
      title: workspace.title,
      turn_count: 3,
      subagents: [
        { call_id: "toolu_task_1", name: "Find the parser", status: "done" },
        { call_id: "toolu_task_2", name: "Run the suite", status: "failed" },
      ],
    };
    render(
      <WorkspaceCard
        workspace={workspace}
        digest={digest}
        session={undefined}
        repoName="app"
        active={false}
        terminalOpen={false}
        density="detailed"
        visibleMeta={{ repoChip: true, branch: false }}
        commands={workspaceCommands({ hasPr: false, archived: false })}
        onOpen={vi.fn()}
        onOpenSubagent={vi.fn()}
        onCommand={vi.fn()}
      />,
    );

    const toggle = screen.getByRole("button", { name: /2 subagents for/ });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(
      screen.queryByRole("button", { name: /Subagent for Fix login/ }),
    ).not.toBeInTheDocument();
    expect(
      document.querySelector('[data-state-glyph="done"]'),
    ).toBeInTheDocument();

    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByRole("button", {
        name: "Subagent for Fix login: Run the suite, Failed",
      }),
    ).toBeInTheDocument();
  });
});
