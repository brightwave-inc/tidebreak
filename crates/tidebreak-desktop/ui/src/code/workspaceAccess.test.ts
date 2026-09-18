import { describe, expect, it } from "vitest";

import { codeWorkspace } from "@/stories/fixtures";
import { workspaceCommandsForAccess } from "./workspaceAccess";

describe("workspaceCommandsForAccess", () => {
  it("keeps restore for a remote sandbox workspace and hides host tools", () => {
    const commands = workspaceCommandsForAccess(
      {
        ...codeWorkspace,
        worktree_path: "remote:ws-1",
        read_only: false,
        is_owner: true,
      },
      [
        { id: "restore", label: "Restore workspace" },
        { id: "archive", label: "Archive workspace" },
        { id: "toggle-terminal", label: "Toggle terminal" },
        { id: "retry-setup", label: "Retry setup" },
        { id: "open-worktree", label: "Open worktree folder" },
      ],
    );
    expect(commands.map((command) => command.id)).toEqual([
      "restore",
      "archive",
    ]);
  });
});
