import { describe, expect, it } from "vitest";

import type { CodeRepoSnapshot, CodeWorkspaceSnapshot } from "../api/types";
import { groupWorkspacesByRepo, prChipTone } from "./workspaceCards";

function repo(id: string): CodeRepoSnapshot {
  return {
    id,
    root_path: `/tmp/${id}`,
    display_name: id,
    default_base_ref: "main",
    branch_prefix: "tidebreak",
    quick_actions: [],
    created_at: "2026-08-15T00:00:00.000Z",
  };
}

function workspace(
  id: string,
  repoId: string,
  status: CodeWorkspaceSnapshot["status"] = "active",
): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: repoId,
    title: id,
    worktree_path: `/tmp/${repoId}/.worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status,
    created_at: "2026-08-15T00:00:00.000Z",
  };
}

describe("groupWorkspacesByRepo", () => {
  it("groups in repo order, drops archived, and keeps orphans visible", () => {
    const groups = groupWorkspacesByRepo(
      [repo("app"), repo("lib"), repo("empty")],
      [
        workspace("ws-lib", "lib"),
        workspace("ws-app-1", "app"),
        workspace("ws-archived", "app", "archived"),
        workspace("ws-app-2", "app"),
        workspace("ws-orphan", "gone"),
      ],
    );

    expect(
      groups.map((group) => [
        group.repo?.id ?? null,
        group.workspaces.map((item) => item.id),
      ]),
    ).toEqual([
      ["app", ["ws-app-1", "ws-app-2"]],
      ["lib", ["ws-lib"]],
      [null, ["ws-orphan"]],
    ]);
  });
});

describe("prChipTone", () => {
  it("maps host state tokens case-insensitively and defaults unknowns", () => {
    expect(prChipTone("OPEN")).toBe("open");
    expect(prChipTone("draft")).toBe("draft");
    expect(prChipTone("Merged")).toBe("merged");
    expect(prChipTone("closed")).toBe("closed");
    expect(prChipTone("locked")).toBe("other");
  });
});
