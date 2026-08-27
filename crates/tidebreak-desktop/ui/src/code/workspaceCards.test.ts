import { describe, expect, it } from "vitest";

import type {
  Attention,
  CodeRepoSnapshot,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
} from "../api/types";
import {
  arrangeWorkspaces,
  formatCompactAge,
  groupWorkspacesByRepo,
  listArchivedWorkspaces,
  middleTruncate,
  isSessionRowWorthy,
  sessionActivityLabel,
  sessionActivityLineLabel,
  sessionRowLabel,
  workspaceCardLabel,
  workspacePrChipSummary,
  workspaceStackParent,
  workspaceStatusRank,
} from "./workspaceCards";
import {
  prCompactStatusLabel,
  prCompactStatusTone,
  pullRequestLifecycle,
} from "./prState";

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
  createdAt = "2026-08-15T00:00:00.000Z",
): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: repoId,
    title: id,
    worktree_path: `/tmp/${repoId}/.worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status,
    created_at: createdAt,
  };
}

const working: Attention = { state: { type: "working" }, source: "lifecycle" };

describe("compact PR status", () => {
  it("uses the host merge-queue color before the open lifecycle color", () => {
    expect(
      prCompactStatusTone({
        state: "open",
        draft: false,
        in_merge_queue: true,
      }),
    ).toBe("pending");
    expect(prCompactStatusTone({ state: "open", draft: false })).toBe("ready");
    expect(
      prCompactStatusLabel({
        state: "open",
        draft: false,
        in_merge_queue: true,
      }),
    ).toBe("In merge queue");
  });
});

function digest(
  workspaceId: string,
  overrides: Partial<CodeSessionDigest> = {},
): CodeSessionDigest {
  return {
    workspace: workspaceId,
    session: `sess-${workspaceId}`,
    kind: "interactive",
    lifecycle: "idle",
    attention: working,
    title: workspaceId,
    turn_count: 0,
    ...overrides,
  };
}

function idsOf(groups: { workspaces: CodeWorkspaceSnapshot[] }[]): string[] {
  return groups.flatMap((group) => group.workspaces.map((item) => item.id));
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

  it("orders a repo's workspaces by created_at, not catalog array order", () => {
    const groups = groupWorkspacesByRepo(
      [repo("app")],
      [
        workspace("ws-new", "app", "active", "2026-08-17T00:00:00.000Z"),
        workspace("ws-old", "app", "active", "2026-08-14T00:00:00.000Z"),
      ],
    );
    expect(groups[0]?.workspaces.map((item) => item.id)).toEqual([
      "ws-old",
      "ws-new",
    ]);
  });
});

describe("arrangeWorkspaces", () => {
  const repos = [repo("app"), repo("lib")];
  const listed = [
    workspace("ws-lib", "lib", "active", "2026-08-16T00:00:00.000Z"),
    workspace("ws-app-new", "app", "active", "2026-08-17T00:00:00.000Z"),
    workspace("ws-app-old", "app", "active", "2026-08-14T00:00:00.000Z"),
    workspace("ws-archived", "app", "archived", "2026-08-13T00:00:00.000Z"),
  ];

  it("groups by repo in creation order within each repo", () => {
    const groups = arrangeWorkspaces("by-repo", repos, listed, {});
    expect(
      groups.map((group) => [
        group.key,
        group.workspaces.map((item) => item.id),
      ]),
    ).toEqual([
      ["app", ["ws-app-old", "ws-app-new"]],
      ["lib", ["ws-lib"]],
    ]);
  });

  it("groups by status rank and keeps created_at order inside a rank", () => {
    const digests = {
      "ws-app-old": digest("ws-app-old", {
        attention: {
          state: {
            type: "needs_you",
            prompt: "an approval is waiting",
            source: "structured",
          },
          source: "structured",
        },
      }),
      "ws-lib": digest("ws-lib", { lifecycle: "running" }),
      "ws-app-new": digest("ws-app-new", {
        pr_state: { number: 12, state: "open" },
      }),
    };
    const groups = arrangeWorkspaces("by-status", repos, listed, digests);
    expect(
      groups.map((group) => [
        group.key,
        group.workspaces.map((item) => item.id),
      ]),
    ).toEqual([
      ["needs_you", ["ws-app-old"]],
      ["running", ["ws-lib"]],
      ["pr_open", ["ws-app-new"]],
    ]);
  });

  it("keeps archived off the rail by default in every mode", () => {
    for (const mode of ["by-repo", "by-status", "by-created"] as const) {
      const groups = arrangeWorkspaces(mode, repos, listed, {});
      expect(idsOf(groups)).not.toContain("ws-archived");
    }
  });

  it("keeps archive ordering available to the dedicated archive page", () => {
    const rows = [
      ...listed,
      {
        ...workspace(
          "ws-archived-late",
          "lib",
          "archived",
          "2026-08-10T00:00:00.000Z",
        ),
        archived_at: "2026-08-18T00:00:00.000Z",
      },
    ];
    // archived_at orders the shelf; rows without it fall back to created_at.
    expect(listArchivedWorkspaces(rows).map((item) => item.id)).toEqual([
      "ws-archived-late",
      "ws-archived",
    ]);
  });

  it("lists by created newest first and hides archived", () => {
    const groups = arrangeWorkspaces("by-created", repos, listed, {});
    expect(groups).toHaveLength(1);
    expect(groups[0]?.label).toBeNull();
    expect(groups[0]?.workspaces.map((item) => item.id)).toEqual([
      "ws-app-new",
      "ws-lib",
      "ws-app-old",
    ]);
  });

  it("selecting a workspace does not reorder", () => {
    const original = [
      workspace("ws-a", "app", "active", "2026-08-14T00:00:00.000Z"),
      workspace("ws-b", "app", "active", "2026-08-16T00:00:00.000Z"),
    ];
    // The old catalog upsert prepended the viewed row. Presentation must
    // ignore that and keep created_at order.
    const afterSelect = [original[1]!, original[0]!];
    const before = arrangeWorkspaces("by-repo", [repo("app")], original, {});
    const after = arrangeWorkspaces("by-repo", [repo("app")], afterSelect, {});
    expect(idsOf(after)).toEqual(idsOf(before));
    expect(idsOf(after)).toEqual(["ws-a", "ws-b"]);

    const digestBefore = {
      "ws-b": digest("ws-b", { turn_count: 1 }),
    };
    const digestAfter = {
      "ws-b": digest("ws-b", { turn_count: 9, title: "renamed" }),
    };
    expect(
      idsOf(
        arrangeWorkspaces("by-created", [repo("app")], original, digestAfter),
      ),
    ).toEqual(
      idsOf(
        arrangeWorkspaces("by-created", [repo("app")], original, digestBefore),
      ),
    );
  });

  it("moves a row under by-status only when the rank itself changes", () => {
    const rows = [
      workspace("ws-a", "app", "active", "2026-08-14T00:00:00.000Z"),
      workspace("ws-b", "app", "active", "2026-08-16T00:00:00.000Z"),
    ];
    const idle = {
      "ws-a": digest("ws-a"),
      "ws-b": digest("ws-b"),
    };
    const bNeedsYou = {
      ...idle,
      "ws-b": digest("ws-b", {
        attention: {
          state: {
            type: "needs_you",
            prompt: "an approval is waiting",
            source: "structured",
          },
          source: "structured",
        },
      }),
    };
    expect(
      idsOf(arrangeWorkspaces("by-status", [repo("app")], rows, idle)),
    ).toEqual(["ws-b", "ws-a"]);
    expect(
      idsOf(arrangeWorkspaces("by-status", [repo("app")], rows, bNeedsYou)),
    ).toEqual(["ws-b", "ws-a"]);
    expect(
      arrangeWorkspaces("by-status", [repo("app")], rows, bNeedsYou).map(
        (group) => group.key,
      ),
    ).toEqual(["needs_you", "idle"]);
  });
});

describe("workspaceStatusRank", () => {
  it("ranks archived last even when a digest is noisy", () => {
    expect(
      workspaceStatusRank(
        workspace("ws-a", "app", "archived"),
        digest("ws-a", { lifecycle: "running" }),
      ),
    ).toBe("archived");
  });
});

describe("pullRequestLifecycle", () => {
  it("maps host state tokens case-insensitively", () => {
    expect(pullRequestLifecycle({ state: "OPEN" })).toBe("open");
    expect(pullRequestLifecycle({ state: "draft", draft: true })).toBe("draft");
    expect(pullRequestLifecycle({ state: "Merged" })).toBe("merged");
    expect(pullRequestLifecycle({ state: "closed" })).toBe("closed");
  });
});

describe("middleTruncate", () => {
  it("keeps the head and tail when the name is long", () => {
    expect(middleTruncate("tidebreak/fix-login-flow", 14)).toBe(
      "tidebre…n-flow",
    );
    expect(middleTruncate("short", 14)).toBe("short");
  });
});

describe("formatCompactAge", () => {
  const now = Date.parse("2026-08-18T12:00:00.000Z");

  it("renders now, minutes, hours, and days", () => {
    expect(formatCompactAge("2026-08-18T11:59:40.000Z", now)).toBe("now");
    expect(formatCompactAge("2026-08-18T11:48:00.000Z", now)).toBe("12m");
    expect(formatCompactAge("2026-08-18T09:00:00.000Z", now)).toBe("3h");
    expect(formatCompactAge("2026-08-16T12:00:00.000Z", now)).toBe("2d");
  });
});

describe("isSessionRowWorthy", () => {
  it("keeps a parked idle agent on the rail", () => {
    expect(isSessionRowWorthy(digest("ws-a", { turn_count: 2 }))).toBe(true);
    expect(
      isSessionRowWorthy(
        digest("ws-a", { lifecycle: "created", turn_count: 0 }),
      ),
    ).toBe(false);
    expect(isSessionRowWorthy(undefined)).toBe(false);
  });
});

describe("sessionRowLabel", () => {
  it("calls a parked turn Done once it has finished work", () => {
    expect(sessionRowLabel(digest("ws-a", { turn_count: 2 }))).toBe("Done");
    expect(
      sessionRowLabel(digest("ws-a", { lifecycle: "created", turn_count: 0 })),
    ).toBe("Created");
  });
});

describe("sessionActivityLineLabel", () => {
  it("prefers the recap on a parked turn", () => {
    expect(
      sessionActivityLineLabel(
        digest("ws-a", {
          turn_count: 4,
          recap: "Folded the backoff into refresh.",
        }),
      ),
    ).toBe("Folded the backoff into refresh.");
  });

  it("keeps live activity and the turn count together", () => {
    expect(
      sessionActivityLineLabel(
        digest("ws-a", {
          lifecycle: "running",
          activity: "shell",
          turn_count: 3,
        }),
      ),
    ).toBe("Shell running · 3 turns");
  });

  it("falls back to Done and the turn count without a recap", () => {
    expect(sessionActivityLineLabel(digest("ws-a", { turn_count: 2 }))).toBe(
      "Done · 2 turns",
    );
  });
});

describe("sessionActivityLabel", () => {
  it.each([
    ["agent", "Agent working"],
    ["shell", "Shell running"],
    ["monitor", "Monitoring"],
    ["file", "Working with files"],
    ["search", "Searching"],
    ["tool", "Tool running"],
  ] as const)("labels %s activity precisely", (activity, label) => {
    expect(
      sessionActivityLabel(digest("ws-a", { lifecycle: "running", activity })),
    ).toBe(label);
  });

  it("counts running subagents and ignores settled ones", () => {
    expect(
      sessionActivityLabel(
        digest("ws-a", {
          lifecycle: "running",
          activity: "subagents",
          subagents: [
            { call_id: "task-1", name: "Inspect parser", status: "running" },
            { call_id: "task-2", name: "Run tests", status: "running" },
            { call_id: "task-3", name: "Map UI", status: "done" },
          ],
        }),
      ),
    ).toBe("2 subagents working");
  });

  it("keeps the generic fallback for older running digests", () => {
    expect(sessionActivityLabel(digest("ws-a", { lifecycle: "running" }))).toBe(
      "Agent working",
    );
  });
});

describe("workspaceCardLabel", () => {
  it("carries the state the glyph rail shows, in card order", () => {
    expect(
      workspaceCardLabel({
        title: "Fix login",
        repoName: "app",
        branchName: "tidebreak/fix-login",
        attention: {
          state: {
            type: "needs_you",
            prompt: "Run this command?",
            source: "structured",
          },
          source: "structured",
        },
        pr: { number: 12, state: "open" },
        terminalOpen: true,
      }),
    ).toBe(
      "Fix login · Run this command? · Pull request #12 Open · Terminal open · app · tidebreak/fix-login",
    );
  });

  it("says nothing about state a working session does not have", () => {
    expect(
      workspaceCardLabel({
        title: "Fix login",
        repoName: "app",
        branchName: "tidebreak/fix-login",
        attention: { state: { type: "working" }, source: "lifecycle" },
      }),
    ).toBe("Fix login · app · tidebreak/fix-login");
  });

  it("announces lifecycle when an older resting row still says Working", () => {
    const session = digest("ws-a", {
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
    });
    expect(
      workspaceCardLabel({
        title: "Fix login",
        repoName: "app",
        branchName: "tidebreak/fix-login",
        attention: session.attention,
        session,
      }),
    ).toBe("Fix login · Idle · app · tidebreak/fix-login");
  });

  it("announces Done for a parked idle session, matching the complete mark", () => {
    const session = digest("ws-a", {
      lifecycle: "idle",
      attention: { state: { type: "idle" }, source: "lifecycle" },
      turn_count: 4,
    });
    expect(
      workspaceCardLabel({
        title: "Fix login",
        repoName: "app",
        branchName: "tidebreak/fix-login",
        attention: {
          state: { type: "done_unreviewed" },
          source: "lifecycle",
        },
        session,
      }),
    ).toBe("Fix login · Done · app · tidebreak/fix-login");
    expect(
      workspaceCardLabel({
        title: "Fix login",
        repoName: "app",
        branchName: "tidebreak/fix-login",
        attention: session.attention,
        session,
      }),
    ).toBe("Fix login · Done · app · tidebreak/fix-login");
  });

  it("announces queue membership instead of the open lifecycle", () => {
    expect(
      workspaceCardLabel({
        title: "Fix login",
        repoName: "app",
        branchName: "tidebreak/fix-login",
        pr: { number: 12, state: "open", in_merge_queue: true },
      }),
    ).toContain("Pull request #12 In merge queue");
  });
});

describe("workspacePrChipSummary", () => {
  it("stays quiet for one or no attributed pull requests", () => {
    expect(workspacePrChipSummary(undefined)).toBeNull();
    expect(workspacePrChipSummary(0)).toBeNull();
    expect(workspacePrChipSummary(1)).toBeNull();
  });

  it("counts once the workspace worked on several", () => {
    expect(workspacePrChipSummary(2)).toBe("2 PRs");
    expect(workspacePrChipSummary(7)).toBe("7 PRs");
  });
});

describe("workspaceStackParent", () => {
  const parent = {
    id: "ws-parent",
    repo_id: "repo-1",
    branch_name: "tidebreak/base-work",
    title: "Base work",
  };

  it("finds the sibling whose branch this base ref names", () => {
    const child = {
      id: "ws-child",
      repo_id: "repo-1",
      base_ref: "origin/tidebreak/base-work",
    };
    expect(workspaceStackParent(child, [parent])).toEqual({
      id: "ws-parent",
      title: "Base work",
    });
    expect(
      workspaceStackParent({ ...child, base_ref: "tidebreak/base-work" }, [
        parent,
      ]),
    ).toEqual({ id: "ws-parent", title: "Base work" });
  });

  it("never matches itself, another repo, or a plain default base", () => {
    expect(
      workspaceStackParent(
        { id: "ws-parent", repo_id: "repo-1", base_ref: "tidebreak/base-work" },
        [parent],
      ),
    ).toBeNull();
    expect(
      workspaceStackParent(
        { id: "ws-child", repo_id: "repo-2", base_ref: "tidebreak/base-work" },
        [parent],
      ),
    ).toBeNull();
    expect(
      workspaceStackParent(
        { id: "ws-child", repo_id: "repo-1", base_ref: "origin/main" },
        [parent],
      ),
    ).toBeNull();
  });
});
