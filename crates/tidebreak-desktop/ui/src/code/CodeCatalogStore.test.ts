import { afterEach, describe, expect, it } from "vitest";

import type { CodeWorkspaceSnapshot } from "../api/types";
import { useCodeCatalogStore } from "./CodeCatalogStore";

function workspace(
  id: string,
  createdAt: string,
  title = id,
): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: "repo-1",
    title,
    worktree_path: `/tmp/app/.worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status: "active",
    created_at: createdAt,
  };
}

afterEach(() => {
  useCodeCatalogStore.getState().reset();
});

describe("CodeCatalogStore.upsertWorkspace", () => {
  it("selecting a workspace does not reorder the catalog", () => {
    const first = workspace("ws-a", "2026-08-14T00:00:00.000Z");
    const second = workspace("ws-b", "2026-08-16T00:00:00.000Z");
    useCodeCatalogStore.setState({ workspaces: [first, second] });

    useCodeCatalogStore.getState().upsertWorkspace({
      ...second,
      title: "viewed",
    });

    expect(
      useCodeCatalogStore.getState().workspaces.map((item) => item.id),
    ).toEqual(["ws-a", "ws-b"]);
    expect(useCodeCatalogStore.getState().workspaces[1]?.title).toBe("viewed");
  });
});
