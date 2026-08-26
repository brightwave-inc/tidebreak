import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke, isTauri: () => true }));

import {
  CodeWorktreeOpenError,
  codeWorktreeOpenFailureMessage,
  codeWorktreePlatform,
  openCodeWorktree,
  parseCodeWorktreeOpenError,
} from "./codeWorktreeHost";

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
