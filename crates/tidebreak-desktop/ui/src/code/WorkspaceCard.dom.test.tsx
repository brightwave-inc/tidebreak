// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { workspaceCommands } from "./workspaceActions";
import { WorkspaceCard } from "./WorkspaceCard";
import type {
  CodeSessionDigest,
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";

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
  workspace?: Partial<CodeWorkspaceSnapshot>;
  pr?: PullRequestDigest;
  density?: "compact" | "detailed";
  visibleMeta?: { repoChip: boolean; branch: boolean };
  detailDefaultOpen?: boolean;
}) {
  const onOpen = vi.fn();
  const onCommand = vi.fn();
  const merged = {
    ...workspace,
    ...overrides?.workspace,
    pr: overrides?.pr,
  };
  render(
    <WorkspaceCard
      workspace={merged}
      digest={undefined}
      session={undefined}
      repoName="app"
      active={false}
      terminalOpen={false}
      density={overrides?.density ?? "detailed"}
      visibleMeta={overrides?.visibleMeta ?? { repoChip: true, branch: false }}
      commands={workspaceCommands({
        hasPr: Boolean(overrides?.pr),
        archived: merged.status === "archived",
      })}
      detailDefaultOpen={overrides?.detailDefaultOpen}
      onOpen={onOpen}
      onCommand={onCommand}
    />,
  );
  return { onOpen, onCommand };
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
      name: "Fix login · Creating workspace · app",
    });
    expect(row).toBeDisabled();
    expect(screen.getByText("Creating workspace")).toBeInTheDocument();
    await user.click(row);
    expect(onOpen).not.toHaveBeenCalled();
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

  it("keeps idle lifecycle detail off the non-hover card", () => {
    const session: CodeSessionSnapshot = {
      id: "sess-1",
      workspace_id: workspace.id,
      kind: "interactive",
      harness_kind: "claude_code",
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
        digest={undefined}
        session={session}
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

    expect(screen.queryByText("Idle")).not.toBeInTheDocument();
    expect(screen.queryByTitle("Claude Code")).not.toBeInTheDocument();
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
    ["shell", "Shell running · 3 turns"],
    ["monitor", "Monitoring · 3 turns"],
  ] as const)(
    "renders %s activity as its own workspace state",
    (activity, label) => {
      const digest: CodeSessionDigest = {
        workspace: workspace.id,
        session: "sess-1",
        kind: "interactive",
        lifecycle: "running",
        attention: { state: { type: "working" }, source: "lifecycle" },
        title: workspace.title,
        turn_count: 3,
        activity,
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
    },
  );

  it("detailed running cards lead with the harness mark and turn count", () => {
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
    expect(screen.getByText("Agent working · 1 turn")).toBeInTheDocument();
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
    expect(screen.getByText("Agent working · 1 turn")).toBeInTheDocument();
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
});
