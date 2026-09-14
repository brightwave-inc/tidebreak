import { describe, expect, it, vi } from "vitest";
import { HttpError } from "../api/client";
import type { CodeWorkspaceSnapshot } from "../api/types";
import { bulkArchiveWorkspaces } from "./bulkWorkspaceArchive";

function workspace(id: string): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: "repo-1",
    title: `Workspace ${id}`,
    worktree_path: `/tmp/worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status: "active",
    created_at: "2026-09-14T00:00:00Z",
  };
}

function archived(id: string): CodeWorkspaceSnapshot {
  return { ...workspace(id), status: "released" };
}

const leftover = (kind = "uncommitted") =>
  new HttpError(409, "409: workspace has leftover work", kind);

describe("bulkArchiveWorkspaces", () => {
  it("archives clean workspaces and asks once before retrying all discard blockers", async () => {
    const client = {
      archiveCodeWorkspace: vi.fn(async (id: string, force: boolean) => {
        if (id !== "clean" && !force)
          throw leftover(id === "dirty" ? "uncommitted" : "ignored_content");
        return archived(id);
      }),
    };
    const confirm = vi.fn(async () => true);
    const onArchived = vi.fn();
    const onProgress = vi.fn();
    const result = await bulkArchiveWorkspaces({
      client,
      workspaces: [
        workspace("clean"),
        workspace("dirty"),
        workspace("ignored"),
      ],
      force: false,
      confirm,
      onArchived,
      onProgress,
    });
    expect(client.archiveCodeWorkspace.mock.calls).toEqual([
      ["clean", false],
      ["dirty", false],
      ["ignored", false],
      ["dirty", true],
      ["ignored", true],
    ]);
    expect(confirm).toHaveBeenCalledOnce();
    expect(confirm).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Discard leftover work in 2 workspaces?",
        destructive: true,
      }),
    );
    expect([...result.archivedIds]).toEqual(["clean", "dirty", "ignored"]);
    expect(result.failed).toEqual([]);
    expect(onArchived.mock.calls.map(([row]) => row.id)).toEqual([
      "clean",
      "dirty",
      "ignored",
    ]);
    expect(onProgress.mock.calls).toEqual([
      [0, 3],
      [1, 3],
      [2, 3],
      [3, 3],
      [0, 2],
      [1, 2],
      [2, 2],
    ]);
  });

  it("preserves blocked workspaces when you cancel, without treating cancellation as failure", async () => {
    const client = {
      archiveCodeWorkspace: vi.fn(async (id: string) => {
        if (id === "dirty") throw leftover();
        return archived(id);
      }),
    };
    const result = await bulkArchiveWorkspaces({
      client,
      workspaces: [workspace("dirty"), workspace("clean")],
      force: false,
      confirm: vi.fn(async () => false),
      onArchived: vi.fn(),
    });
    expect(client.archiveCodeWorkspace).toHaveBeenCalledTimes(2);
    expect([...result.archivedIds]).toEqual(["clean"]);
    expect(result.failed).toEqual([]);
  });

  it("preserves failure reasons and never force-retries other errors", async () => {
    const errors = [
      new HttpError(
        409,
        "409: a workspace terminal did not stop",
        "terminal_shutdown_timeout",
      ),
      new HttpError(422, "422: backup script failed", "archive_script_failed"),
      new Error("Connection lost"),
    ];
    const client = {
      archiveCodeWorkspace: vi.fn(async (id: string) => {
        if (id !== "clean") throw errors[Number(id)];
        return archived(id);
      }),
    };
    const confirm = vi.fn(async () => true);
    const result = await bulkArchiveWorkspaces({
      client,
      workspaces: [
        ...errors.map((_, index) => workspace(String(index))),
        workspace("clean"),
      ],
      force: false,
      confirm,
      onArchived: vi.fn(),
    });
    expect(confirm).not.toHaveBeenCalled();
    expect(client.archiveCodeWorkspace).toHaveBeenCalledTimes(4);
    expect(result.failed.map(({ error }) => error)).toEqual(errors);
    expect([...result.archivedIds]).toEqual(["clean"]);
  });

  it("reports a failed force retry and still archives the next blocked workspace", async () => {
    const failure = new Error("Backup failed");
    const client = {
      archiveCodeWorkspace: vi.fn(async (id: string, force: boolean) => {
        if (!force) throw leftover("unpushed");
        if (id === "failed") throw failure;
        return archived(id);
      }),
    };
    const result = await bulkArchiveWorkspaces({
      client,
      workspaces: [workspace("failed"), workspace("ok")],
      force: false,
      confirm: vi.fn(async () => true),
      onArchived: vi.fn(),
    });
    expect(result.failed).toEqual([
      { workspace: workspace("failed"), error: failure },
    ]);
    expect([...result.archivedIds]).toEqual(["ok"]);
  });

  it("does not ask again or retry when the batch already has force permission", async () => {
    const client = {
      archiveCodeWorkspace: vi.fn(async (id: string) => {
        if (id === "failed") throw leftover();
        return archived(id);
      }),
    };
    const confirm = vi.fn(async () => true);
    const result = await bulkArchiveWorkspaces({
      client,
      workspaces: [workspace("failed"), workspace("ok")],
      force: true,
      confirm,
      onArchived: vi.fn(),
    });
    expect(client.archiveCodeWorkspace.mock.calls).toEqual([
      ["failed", true],
      ["ok", true],
    ]);
    expect(confirm).not.toHaveBeenCalled();
    expect(result.failed).toHaveLength(1);
    expect([...result.archivedIds]).toEqual(["ok"]);
  });

  it("waits for each archive before starting another in the same repository", async () => {
    let release!: (row: CodeWorkspaceSnapshot) => void;
    const first = new Promise<CodeWorkspaceSnapshot>((resolve) => {
      release = resolve;
    });
    const client = {
      archiveCodeWorkspace: vi.fn(async (id: string) =>
        id === "first" ? first : archived(id),
      ),
    };
    const result = bulkArchiveWorkspaces({
      client,
      workspaces: [workspace("first"), workspace("second")],
      force: false,
      confirm: vi.fn(async () => true),
      onArchived: vi.fn(),
    });
    expect(client.archiveCodeWorkspace).toHaveBeenCalledTimes(1);
    release(archived("first"));
    expect([...(await result).archivedIds]).toEqual(["first", "second"]);
  });
});
