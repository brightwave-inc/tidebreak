// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { workspaceCommands } from "./workspaceActions";
import { WorkspaceCard } from "./WorkspaceCard";
import type {
  CodeSessionDigest,
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
      screen.getByRole("button", { name: "Fix login · app · tidebreak/fix-login" }),
    );
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("keeps one menu: right-click carries every command", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({ pr });

    fireEvent.contextMenu(
      screen.getByRole("button", { name: /^Fix login/ }),
    );
    const menu = await screen.findByRole("menu");
    expect(menu).toHaveTextContent("Open pull request");
    expect(menu).toHaveTextContent("Archive");
    await user.click(
      screen.getByRole("menuitem", { name: "Toggle terminal" }),
    );
    expect(onCommand).toHaveBeenCalledWith("toggle-terminal");
  });

  it("puts the PR and its action in the detail panel, not the row", async () => {
    const user = userEvent.setup();
    const { onOpen, onCommand } = renderCard({ pr, detailDefaultOpen: true });

    // The panel says what the row cannot: full branch, checks, the PR.
    expect(screen.getByText("tidebreak/fix-login")).toBeInTheDocument();
    expect(screen.getByText("8 passing, 1 pending")).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Open pull request #184" }),
    );
    expect(onCommand).toHaveBeenCalledWith("open-pr");
    // Panel actions never double as "open the workspace".
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("leads an archived workspace's panel with Restore", async () => {
    const user = userEvent.setup();
    const { onCommand } = renderCard({
      workspace: { status: "archived", archived_at: "2026-08-18T00:00:00.000Z" },
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
    expect(
      screen.queryByText("tidebreak/fix-login"),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Fix login · app · tidebreak/fix-login" }),
    ).toBeInTheDocument();
  });

  it("compact keeps the one-line read and the complete label", () => {
    renderCard({ density: "compact" });

    expect(screen.queryByText("app")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Fix login · app · tidebreak/fix-login" }),
    ).toBeInTheDocument();
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
      screen.getByRole("button", { name: "Watch task for Fix login: Running" }),
    );
    expect(onOpenChildSession).toHaveBeenCalledWith("sess-watch");
    // The child row is a sibling of the row button; opening it must not
    // also open the workspace.
    expect(onOpen).not.toHaveBeenCalled();
  });
});
