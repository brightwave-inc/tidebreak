import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke, isTauri: () => true }));

import {
  CodeEditorOpenError,
  CodeWorktreeOpenError,
  codeEditorOpenFailureMessage,
  codeWorktreeOpenFailureMessage,
  codeWorktreePlatform,
  detectExternalEditors,
  openCodeWorktree,
  openInEditor,
  parseCodeEditorOpenError,
  parseCodeWorktreeOpenError,
} from "./codeWorktreeHost";
import { setEditorPreference } from "./editorPreference";

beforeEach(() => invoke.mockReset());

describe("code worktree host", () => {
  it("uses the narrow native command and validates its result", async () => {
    invoke.mockResolvedValue({ status: "opened" });

    await openCodeWorktree("e777ae2f-619c-4ed7-ad1f-8c9dfd179a90");

    expect(invoke).toHaveBeenCalledWith("open_code_worktree", {
      workspaceId: "e777ae2f-619c-4ed7-ad1f-8c9dfd179a90",
    });
    invoke.mockResolvedValue({ status: "unexpected" });
    await expect(
      openCodeWorktree("e777ae2f-619c-4ed7-ad1f-8c9dfd179a90"),
    ).rejects.toMatchObject({ reason: "code_worktree_open_failed" });
  });

  it("keeps native failures typed and keeps their detail out of user copy", () => {
    const error = parseCodeWorktreeOpenError({
      reason: "code_worktree_path_not_found",
      detail: "private native diagnostic",
    });

    expect(error).toBeInstanceOf(CodeWorktreeOpenError);
    expect(error.reason).toBe("code_worktree_path_not_found");
    expect(error.detail).toBe("private native diagnostic");
    expect(
      codeWorktreeOpenFailureMessage({
        reason: "code_worktree_path_not_found",
        detail: "private native diagnostic",
      }),
    ).toBe(
      "The worktree folder no longer exists. Refresh or restore the workspace, then try again.",
    );
  });

  it("falls back to a typed generic failure for malformed rejections", () => {
    expect(parseCodeWorktreeOpenError("native stack trace")).toMatchObject({
      reason: "code_worktree_open_failed",
      detail: null,
    });
  });

  it("recognizes only the desktop platforms with a native folder launcher", () => {
    expect(
      codeWorktreePlatform("Mozilla/5.0 (Macintosh; Intel Mac OS X)"),
    ).toBe("macos");
    expect(codeWorktreePlatform("Mozilla/5.0 (Windows NT 10.0; Win64)")).toBe(
      "windows",
    );
    expect(codeWorktreePlatform("Mozilla/5.0 (X11; Linux x86_64)")).toBe(
      "linux",
    );
    expect(codeWorktreePlatform("Tidebreak mobile")).toBe("unsupported");
  });
});

describe("open in editor host", () => {
  it("sends the workspace, the relative path, and the chosen editor", async () => {
    setEditorPreference({ editor: "zed", customProgram: "" });
    invoke.mockResolvedValue({ status: "opened" });

    await openInEditor({
      workspaceId: "e777ae2f-619c-4ed7-ad1f-8c9dfd179a90",
      relativePath: "crates/tidebreak-desktop/src/lib.rs",
      line: 42,
    });

    expect(invoke).toHaveBeenCalledWith("open_in_editor", {
      request: {
        workspaceId: "e777ae2f-619c-4ed7-ad1f-8c9dfd179a90",
        relativePath: "crates/tidebreak-desktop/src/lib.rs",
        line: 42,
        editor: "zed",
        customProgram: null,
      },
    });
  });

  it("carries the custom program only when the reader chose one", async () => {
    setEditorPreference({
      editor: "custom",
      customProgram: "/opt/homebrew/bin/nvim",
    });
    invoke.mockResolvedValue({ status: "opened" });

    await openInEditor({ workspaceId: "ws-1" });

    expect(invoke).toHaveBeenCalledWith("open_in_editor", {
      request: {
        workspaceId: "ws-1",
        relativePath: null,
        line: null,
        editor: "custom",
        customProgram: "/opt/homebrew/bin/nvim",
      },
    });
  });

  it("refuses a result that is not the one opened shape", async () => {
    setEditorPreference({ editor: "vscode", customProgram: "" });
    invoke.mockResolvedValue({ status: "maybe" });

    await expect(openInEditor({ workspaceId: "ws-1" })).rejects.toMatchObject({
      reason: "code_editor_open_failed",
    });
  });

  it("keeps native failures typed and their detail out of user copy", () => {
    const error = parseCodeEditorOpenError({
      reason: "code_editor_path_outside_worktree",
      detail: "/etc/passwd",
    });

    expect(error).toBeInstanceOf(CodeEditorOpenError);
    expect(error.reason).toBe("code_editor_path_outside_worktree");
    expect(error.detail).toBe("/etc/passwd");
    expect(
      codeEditorOpenFailureMessage({
        reason: "code_editor_path_outside_worktree",
      }),
    ).toBe("That path does not sit inside this workspace's worktree.");
    expect(parseCodeEditorOpenError("native stack trace")).toMatchObject({
      reason: "code_editor_open_failed",
      detail: null,
    });
    // A worktree reason is not an editor reason: the two sets stay separate.
    expect(
      parseCodeEditorOpenError({ reason: "code_worktree_path_not_found" })
        .reason,
    ).toBe("code_editor_open_failed");
    expect(
      parseCodeWorktreeOpenError({ reason: "code_editor_open_failed" }).reason,
    ).toBe("code_worktree_open_failed");
  });

  it("keeps only well-formed probes from the native detection", async () => {
    invoke.mockResolvedValue([
      { id: "vscode", program: "/usr/local/bin/code" },
      { id: "zed", program: null },
      { id: "cursor" },
      "not a probe",
    ]);

    expect(await detectExternalEditors()).toEqual([
      { id: "vscode", program: "/usr/local/bin/code" },
      { id: "zed", program: null },
    ]);
  });
});
