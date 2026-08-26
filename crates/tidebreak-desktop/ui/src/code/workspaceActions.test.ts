import { describe, expect, it } from "vitest";

import {
  workspaceCommands,
  workspaceHeaderCommands,
  worktreeOpenFailureNotice,
} from "./workspaceActions";

describe("workspace worktree actions", () => {
  it("offers a native folder action only for an active local workspace", () => {
    const local = workspaceCommands({
      hasPr: false,
      archived: false,
      canOpenWorktree: true,
    });
    expect(local).toContainEqual({
      id: "open-worktree",
      label: "Open worktree folder",
    });
    expect(local.some((command) => command.id === "copy-worktree")).toBe(false);

    const fallback = workspaceCommands({
      hasPr: false,
      archived: false,
      canOpenWorktree: false,
    });
    expect(fallback).toContainEqual({
      id: "copy-worktree",
      label: "Copy worktree path",
    });
    expect(fallback.some((command) => command.id === "open-worktree")).toBe(
      false,
    );

    const archived = workspaceCommands({
      hasPr: false,
      archived: true,
      canOpenWorktree: true,
    });
    expect(archived.some((command) => command.id === "open-worktree")).toBe(
      false,
    );
    expect(archived.some((command) => command.id === "copy-worktree")).toBe(
      true,
    );
  });

  it("puts the same truthful action in the workspace header overflow", () => {
    const common = {
      archived: false,
      hasSession: false,
      attentionPinned: false,
      quickActions: [],
    };
    expect(
      workspaceHeaderCommands({ ...common, canOpenWorktree: true })[0],
    ).toEqual({ id: "open-worktree", label: "Open worktree folder" });
    expect(
      workspaceHeaderCommands({ ...common, canOpenWorktree: false })[0],
    ).toEqual({ id: "copy-worktree", label: "Copy worktree path" });
    expect(
      workspaceHeaderCommands({
        ...common,
        archived: true,
        canOpenWorktree: true,
      })[0],
    ).toEqual({ id: "copy-worktree", label: "Copy worktree path" });
  });

  it("turns a typed failure into a recoverable notice", () => {
    expect(
      worktreeOpenFailureNotice({
        reason: "code_worktree_path_not_found",
        detail: "/private/native/detail",
      }),
    ).toEqual({
      title: "Could not open worktree folder",
      description:
        "The worktree folder no longer exists. Refresh or restore the workspace, then try again.",
      actionLabel: "Copy path",
    });
  });
});
