import { describe, expect, it } from "vitest";

import type {
  Attention,
  CodeSessionDigest,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "../api/types";
import {
  codeHomeSections,
  deliveryPullRequestTarget,
  type CodeHomeItem,
} from "./codeHomeSections";

const REPOS = [
  { id: "repo-app", display_name: "app" },
  { id: "repo-api", display_name: "api" },
];

const approval: Attention = {
  state: {
    type: "needs_you",
    prompt: "an approval is waiting",
    source: "structured",
  },
  source: "structured",
};
const working: Attention = { state: { type: "working" }, source: "lifecycle" };
const done: Attention = {
  state: { type: "done_unreviewed" },
  source: "lifecycle",
};
const stalled: Attention = {
  state: { type: "stalled", idle_secs: 600 },
  source: "heuristic",
};
const readyNotice: Attention = {
  state: {
    type: "needs_you",
    prompt: "the pull request is ready to merge",
    source: "lifecycle",
  },
  source: "lifecycle",
};

function workspace(
  id: string,
  overrides: Partial<CodeWorkspaceSnapshot> = {},
): CodeWorkspaceSnapshot {
  return {
    id,
    repo_id: "repo-app",
    title: `Workspace ${id}`,
    worktree_path: `/tmp/worktrees/${id}`,
    branch_name: `tidebreak/${id}`,
    base_ref: "main",
    status: "active",
    created_at: "2026-09-01T00:00:00.000Z",
    ...overrides,
  };
}

function digest(
  workspaceId: string | null,
  overrides: Partial<CodeSessionDigest> = {},
): CodeSessionDigest {
  return {
    workspace: workspaceId,
    session: `sess-${workspaceId ?? "free"}`,
    kind: "interactive",
    lifecycle: "idle",
    attention: done,
    title: "",
    turn_count: 3,
    trigger_target_at: "2026-09-20T10:00:00.000Z",
    ...overrides,
  };
}

function pr(overrides: Partial<PullRequestDigest> = {}): PullRequestDigest {
  const number = overrides.number ?? 41;
  return {
    number,
    url: `https://github.com/acme/app/pull/${number}`,
    state: "open",
    title: "Tighten the retry loop",
    ...overrides,
  };
}

const READY: Partial<PullRequestDigest> = {
  mergeable: "mergeable",
  merge_state_status: "clean",
  check_counts: { passing: 6, pending: 0, failing: 0, skipped: 0 },
};

const FAILING: Partial<PullRequestDigest> = {
  check_counts: { passing: 5, pending: 0, failing: 1, skipped: 0 },
};

function keys(items: readonly CodeHomeItem[]): string[] {
  return items.map((item) => item.key);
}

describe("codeHomeSections", () => {
  it("files each live workspace under the strongest claim it makes", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("ask"),
        workspace("busy", { repo_id: "repo-api" }),
        workspace("green", { pr: pr(READY) }),
        workspace("red", { pr: pr({ number: 42, ...FAILING }) }),
        workspace("shipped", { pr: pr({ number: 43, state: "merged" }) }),
        workspace("fresh"),
        workspace("shelved", { status: "archived" }),
        workspace("gone", { status: "released" }),
        workspace("forming", { status: "creating" }),
      ],
      digests: {
        ask: digest("ask", { attention: approval }),
        busy: digest("busy", {
          lifecycle: "running",
          attention: working,
          activity: "shell",
          activity_detail: "cargo test -p api",
        }),
      },
    });

    expect(keys(sections.needs_you)).toEqual([
      "workspace:ask",
      "workspace:red",
    ]);
    expect(sections.needs_you[0]!.status).toEqual({
      label: "An approval is waiting",
      tone: "critical",
    });
    expect(sections.needs_you[0]!.activity).toEqual({
      at: "2026-09-20T10:00:00.000Z",
      kind: "activity",
    });
    // The pull request put this row here, so the row opens it in Delivery,
    // under the rail's own lifecycle mark; the gate is said in words.
    expect(sections.needs_you[1]).toMatchObject({
      status: { label: "Checks failed", tone: "critical" },
      reference: { kind: "pull_request", number: 42 },
      glyph: { kind: "pull_request", lifecycle: "open" },
      target: {
        kind: "pull_request",
        repository: { host: "github.com", owner: "acme", name: "app" },
        number: 42,
      },
    });

    expect(keys(sections.running)).toEqual(["workspace:busy"]);
    expect(sections.running[0]).toMatchObject({
      context: "api",
      status: { label: "cargo test -p api", tone: "running", live: true },
      target: { kind: "workspace", workspaceId: "busy" },
    });

    expect(keys(sections.ready_to_merge)).toEqual(["workspace:green"]);
    expect(sections.ready_to_merge[0]).toMatchObject({
      title: "Workspace green",
      status: { label: "6 checks passed", tone: "ready" },
      glyph: { kind: "pull_request", lifecycle: "open" },
      // Nothing has run here yet: the only timestamp is the creation.
      activity: { at: "2026-09-01T00:00:00.000Z", kind: "created" },
      target: {
        kind: "pull_request",
        repository: { host: "github.com", owner: "acme", name: "app" },
        number: 41,
      },
    });

    // Archived, released, and half-created workspaces are not live work.
    expect(keys(sections.recent).sort()).toEqual([
      "workspace:fresh",
      "workspace:shipped",
    ]);
    expect(
      sections.recent.find((item) => item.key === "workspace:shipped")?.status,
    ).toEqual({ label: "Merged", tone: "merged" });
    expect(
      sections.recent.find((item) => item.key === "workspace:fresh")?.status,
    ).toBeNull();
    expect(sections.total).toBe(6);
  });

  it("lets a need outrank a running turn, and a running turn outrank the pull request", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("asks-mid-turn", { pr: pr(READY) }),
        workspace("fixing", { pr: pr(FAILING) }),
      ],
      digests: {
        "asks-mid-turn": digest("asks-mid-turn", {
          lifecycle: "running",
          attention: approval,
        }),
        fixing: digest("fixing", { lifecycle: "running", attention: working }),
      },
    });
    expect(keys(sections.needs_you)).toEqual(["workspace:asks-mid-turn"]);
    expect(keys(sections.running)).toEqual(["workspace:fixing"]);
    expect(sections.ready_to_merge).toEqual([]);
  });

  it("reads readiness from the pull request, not from a stale notice", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        // Checks broke after the watch said ready: the failure wins.
        workspace("broke", { pr: pr(FAILING) }),
        // The host has not said whether it can merge yet: the notice fills in.
        workspace("unsure", { pr: pr({ number: 44 }) }),
        // Merged since the notice: nothing is ready any more.
        workspace("landed", { pr: pr({ number: 45, merged: true }) }),
      ],
      digests: {
        broke: digest("broke", { attention: readyNotice }),
        unsure: digest("unsure", { attention: readyNotice }),
        landed: digest("landed", { attention: readyNotice }),
      },
    });
    expect(keys(sections.needs_you)).toEqual(["workspace:broke"]);
    expect(keys(sections.ready_to_merge)).toEqual(["workspace:unsure"]);
    // No approval and no checks to name: the row falls back to the verdict.
    expect(sections.ready_to_merge[0]!.status?.label).toBe("Ready to merge");
    expect(keys(sections.recent)).toEqual(["workspace:landed"]);
    expect(sections.recent[0]!.status?.label).toBe("Merged");
  });

  it("opens the workspace when the pull request URL cannot address Delivery", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("green", {
          pr: pr({ ...READY, review_decision: "APPROVED", url: null }),
        }),
        workspace("red", { pr: pr({ ...FAILING, url: null }) }),
      ],
      digests: {},
    });
    expect(sections.ready_to_merge[0]!.status?.label).toBe(
      "Approved, 6 checks passed",
    );
    expect(sections.ready_to_merge[0]!.target).toEqual({
      kind: "workspace",
      workspaceId: "green",
    });
    expect(sections.needs_you[0]!.target).toEqual({
      kind: "workspace",
      workspaceId: "red",
    });
  });

  it("files a failed setup under Needs you, below an agent still working", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("broken", { status: "setup_failed" }),
        workspace("pressing-on", { status: "setup_failed" }),
      ],
      digests: {
        "pressing-on": digest("pressing-on", {
          lifecycle: "running",
          attention: working,
        }),
      },
    });
    expect(sections.needs_you).toHaveLength(1);
    expect(sections.needs_you[0]).toMatchObject({
      key: "workspace:broken",
      status: { label: "Setup failed", tone: "critical" },
      target: { kind: "workspace", workspaceId: "broken" },
    });
    expect(keys(sections.running)).toEqual(["workspace:pressing-on"]);
    expect(sections.recent).toEqual([]);
  });

  it("classifies a workspace-less conversation by the same rules as a workspace", () => {
    const runningStall = {
      lifecycle: "running" as const,
      attention: stalled,
      activity: "shell" as const,
      activity_detail: "cargo test",
    };
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("quiet-shell"),
        workspace("notice"),
        workspace("stopped"),
      ],
      digests: {
        "quiet-shell": digest("quiet-shell", runningStall),
        notice: digest("notice", { attention: readyNotice }),
        stopped: digest("stopped", { attention: stalled }),
      },
      conversations: [
        digest(null, { session: "free-quiet-shell", ...runningStall }),
        digest(null, { session: "free-notice", attention: readyNotice }),
        digest(null, { session: "free-stopped", attention: stalled }),
      ],
    });
    // A silent command in a turn that is still running is live work, in the
    // warning tone; it becomes a need only once the turn stops.
    expect(keys(sections.running).sort()).toEqual([
      "session:free-quiet-shell",
      "workspace:quiet-shell",
    ]);
    for (const item of sections.running) {
      expect(item.status).toEqual({
        label: "cargo test",
        tone: "warning",
        live: false,
      });
    }
    // A ready notice with no pull request state to check it against is ready
    // to merge, never a need.
    expect(keys(sections.ready_to_merge).sort()).toEqual([
      "session:free-notice",
      "workspace:notice",
    ]);
    expect(keys(sections.needs_you).sort()).toEqual([
      "session:free-stopped",
      "workspace:stopped",
    ]);
  });

  it("surfaces a stuck watch as a need and a fixing watch as live work", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("stuck", { pr: pr(FAILING) }),
        workspace("mending", { pr: pr({ number: 46, ...FAILING }) }),
      ],
      digests: {},
      watches: {
        stuck: {
          "watch-stuck": digest("stuck", {
            session: "watch-stuck",
            kind: "watch",
            lifecycle: "idle",
            attention: working,
            watch_state: "blocked",
          }),
        },
        mending: {
          "watch-mending": digest("mending", {
            session: "watch-mending",
            kind: "watch",
            lifecycle: "running",
            attention: working,
            watch_state: "fixing",
            watch_cycles: 2,
          }),
        },
      },
    });
    expect(sections.needs_you).toHaveLength(1);
    expect(sections.needs_you[0]).toMatchObject({
      key: "workspace:stuck",
      status: { label: "Watch is blocked", tone: "warning" },
      target: { kind: "workspace", workspaceId: "stuck", task: "watch-stuck" },
    });
    expect(sections.running[0]).toMatchObject({
      key: "workspace:mending",
      status: { label: "Watch: Fixing ×2", live: true },
      target: {
        kind: "workspace",
        workspaceId: "mending",
        task: "watch-mending",
      },
    });
  });

  it("files a stopped, quiet turn as a need in its warning tone", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [workspace("quiet"), workspace("asks")],
      digests: {
        quiet: digest("quiet", {
          attention: stalled,
          trigger_target_at: "2026-09-21T10:00:00.000Z",
        }),
        asks: digest("asks", { attention: approval }),
      },
    });
    // Failures and asks lead; a soft stop follows however recent it is.
    expect(keys(sections.needs_you)).toEqual([
      "workspace:asks",
      "workspace:quiet",
    ]);
    expect(sections.needs_you[1]!.status).toEqual({
      label: "Stalled",
      tone: "warning",
    });
  });

  it("lists conversations without a workspace and keeps children under their parent", () => {
    const parent = digest(null, {
      session: "parent",
      title: "Triage the deploy alert",
      lifecycle: "running",
      attention: working,
      external_origin: {
        channel_kind: "slack",
        external_key: "T1/C42/1726000000.000100",
      },
    });
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [],
      digests: {},
      conversations: [
        parent,
        digest(null, {
          session: "child-busy",
          parent_session: "parent",
          lifecycle: "running",
          attention: working,
        }),
        digest(null, {
          session: "child-asks",
          parent_session: "parent",
          attention: approval,
        }),
      ],
    });
    expect(keys(sections.running)).toEqual(["session:parent"]);
    expect(sections.running[0]).toMatchObject({
      title: "Triage the deploy alert",
      context: "Slack channel",
      target: { kind: "session", sessionId: "parent" },
    });
    expect(keys(sections.needs_you)).toEqual(["session:child-asks"]);
    expect(sections.total).toBe(1);
  });

  it("orders each section newest activity first", () => {
    const sections = codeHomeSections({
      repos: REPOS,
      workspaces: [
        workspace("older", { created_at: "2026-09-01T00:00:00.000Z" }),
        workspace("newer", { created_at: "2026-09-10T00:00:00.000Z" }),
        workspace("touched", { created_at: "2026-08-01T00:00:00.000Z" }),
      ],
      digests: {
        touched: digest("touched", {
          trigger_target_at: "2026-09-22T00:00:00.000Z",
        }),
      },
    });
    expect(keys(sections.recent)).toEqual([
      "workspace:touched",
      "workspace:newer",
      "workspace:older",
    ]);
  });
});

describe("deliveryPullRequestTarget", () => {
  it("reads the repository and number from a pull request page", () => {
    expect(
      deliveryPullRequestTarget({
        number: 7,
        url: "https://git.example.com/platform/gateway/pull/7",
      }),
    ).toEqual({
      kind: "pull_request",
      repository: {
        host: "git.example.com",
        owner: "platform",
        name: "gateway",
      },
      number: 7,
    });
  });

  it.each([
    [undefined],
    [null],
    ["not a url"],
    ["https://github.com/acme/app/issues/7"],
    ["https://github.com/acme/app/pull/8"],
  ])("returns null for %s", (url) => {
    expect(deliveryPullRequestTarget({ number: 7, url })).toBeNull();
  });
});
