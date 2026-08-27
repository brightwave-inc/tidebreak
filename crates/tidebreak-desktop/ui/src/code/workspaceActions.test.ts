import { describe, expect, it } from "vitest";

import { setEditorPreference } from "./editorPreference";
import {
  externalEditorOpenFailureNotice,
  workspaceBulkCommands,
  workspaceCommands,
  workspaceHeaderCommands,
  worktreeOpenFailureNotice,
} from "./workspaceActions";

describe("workspaceBulkCommands", () => {
  it("names the count and keeps force-archive destructive", () => {
    const commands = workspaceBulkCommands(4);
    expect(commands).toEqual([
      { id: "archive", label: "Archive 4 workspaces" },
      {
        id: "force-archive",
        label: "Force archive 4 workspaces",
        destructive: true,
        separated: true,
      },
    ]);
    expect(workspaceBulkCommands(2)[0]?.label).toBe("Archive 2 workspaces");
  });
});

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

  it("offers Uneff me next to Copy debug JSON when Tidebreak is connected", () => {
    const commands = workspaceCommands({
      hasPr: false,
      archived: false,
      hasSession: true,
      canUneff: true,
    });
    const copy = commands.findIndex(
      (command) => command.id === "copy-debug-json",
    );
    const uneff = commands.findIndex((command) => command.id === "uneff-me");
    expect(commands[copy]?.label).toBe("Copy debug JSON");
    expect(commands[uneff]?.label).toBe("Uneff me");
    expect(uneff).toBe(copy + 1);

    expect(
      workspaceCommands({
        hasPr: false,
        archived: false,
        hasSession: true,
      }).some((command) => command.id === "uneff-me"),
    ).toBe(false);

    const header = workspaceHeaderCommands({
      archived: false,
      hasSession: true,
      attentionPinned: false,
      quickActions: [],
      canUneff: true,
    });
    const headerCopy = header.findIndex(
      (command) => command.id === "copy-debug-json",
    );
    expect(header[headerCopy + 1]).toEqual({
      id: "uneff-me",
      label: "Uneff me",
    });
  });

  it("names the chosen editor in both menus, and offers it only locally", () => {
    setEditorPreference({ editor: "zed", customProgram: "" });
    const card = workspaceCommands({
      hasPr: false,
      archived: false,
      canOpenWorktree: true,
      canOpenInEditor: true,
    });
    const worktree = card.findIndex(
      (command) => command.id === "open-worktree",
    );
    expect(card[worktree + 1]).toEqual({
      id: "open-in-editor",
      label: "Open in Zed",
    });

    // A custom command has no name worth advertising.
    setEditorPreference({
      editor: "custom",
      customProgram: "/opt/homebrew/bin/nvim",
    });
    expect(
      workspaceCommands({
        hasPr: false,
        archived: false,
        canOpenInEditor: true,
      }).find((command) => command.id === "open-in-editor"),
    ).toEqual({ id: "open-in-editor", label: "Open in editor" });

    // A window attached to another machine, and an archived workspace, have
    // no local file for an editor here to open.
    for (const commands of [
      workspaceCommands({
        hasPr: false,
        archived: false,
        canOpenInEditor: false,
      }),
      workspaceCommands({
        hasPr: false,
        archived: true,
        canOpenInEditor: true,
      }),
    ]) {
      expect(commands.some((command) => command.id === "open-in-editor")).toBe(
        false,
      );
    }
  });

  it("puts the editor next to the worktree action in the header overflow", () => {
    setEditorPreference({ editor: "vscode", customProgram: "" });
    const common = {
      archived: false,
      hasSession: false,
      attentionPinned: false,
      quickActions: [],
    };
    expect(
      workspaceHeaderCommands({
        ...common,
        canOpenWorktree: true,
        canOpenInEditor: true,
      }).slice(0, 2),
    ).toEqual([
      { id: "open-worktree", label: "Open worktree folder" },
      { id: "open-in-editor", label: "Open in Visual Studio Code" },
    ]);
    expect(
      workspaceHeaderCommands({
        ...common,
        archived: true,
        canOpenInEditor: true,
      }).some((command) => command.id === "open-in-editor"),
    ).toBe(false);
  });

  it("says what to do when the editor does not start", () => {
    expect(
      externalEditorOpenFailureNotice({
        reason: "code_editor_editor_unavailable",
        detail: "/private/native/detail",
      }),
    ).toEqual({
      title: "Could not open that file in your editor",
      description:
        "Tidebreak could not find that editor on this computer. Pick another one in Settings, Coding harnesses.",
    });
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
