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

  it("keeps the PR visible in the row without turning it into workspace navigation", async () => {
    const user = userEvent.setup();
    const { onOpen, onCommand } = renderCard({ pr });

    expect(screen.getByText("#184")).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Open pull request #184" }),
    );
    expect(onCommand).toHaveBeenCalledWith("open-pr");
    // PR actions are siblings of the conversation row, never nested controls.
    expect(onOpen).not.toHaveBeenCalled();
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

  it("detailed idle cards still show the harness and lifecycle", () => {
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

    expect(screen.getByText("Idle")).toBeInTheDocument();
    expect(screen.getByTitle("Claude Code")).toBeInTheDocument();
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
