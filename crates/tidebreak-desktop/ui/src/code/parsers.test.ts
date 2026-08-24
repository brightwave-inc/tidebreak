import { describe, expect, it } from "vitest";

import {
  liveCodeSessions,
  parseCodeAction,
  parseCodeApproval,
  parseCodeCloneDefaults,
  parseCodeCloneJob,
  parseCodeCommit,
  parseCodeDeliveryActionResult,
  parseCodeDeliveryPullRequestDetail,
  parseCodeDeliveryPullRequestsPage,
  parseCodeDeliveryRepositories,
  parseCodeDeliveryRunDetail,
  parseCodeDeliveryRunsPage,
  parseCodeEvent,
  parseCodePrComments,
  parseCodePush,
  parseCodeSession,
  parseCodeSessionList,
  parseCodeSubscriptionUsage,
  parseCodeTerminal,
  parseCodeTerminalList,
  parseCodeTerminalRead,
  parseCodeTurn,
  parseCodeTurnList,
  parseCodeTurnSubmission,
  parseCodeUpdateNotice,
  parseCodeWorkspacePullRequests,
  parseCodeWorkspaceBlob,
  parseCodeWorkspaceDiff,
  parseCodeWorkspaceFiles,
  parseCodeWorkspacePr,
  parseCodeWorkspaceSearch,
  parseCodeWorkspaceTree,
  parseCodeWorktreeRoot,
  parseFenceReason,
  parseCodeCheckLogsSnapshot,
} from "./parsers";

const DELIVERY_CAPABILITY = {
  found: true,
  authenticated: true,
  viewer_login: "mara",
  remediation: "",
};

const DELIVERY_REPOSITORY = {
  host: "github.com",
  owner: "brightwave-inc",
  name: "tidebreak",
  name_with_owner: "brightwave-inc/tidebreak",
  url: "https://github.com/brightwave-inc/tidebreak",
  default_branch: "main",
  tidebreak_repo_id: "repo-1",
};

const DELIVERY_WORKSPACE_LINK = {
  workspace_id: "ws-1",
  repo_id: "repo-1",
  title: "Delivery center",
  branch_name: "thet/delivery-center",
  status: "active",
  exact: true,
};

const DELIVERY_PR = {
  id: "github.com/brightwave-inc/tidebreak#2248",
  repository: DELIVERY_REPOSITORY,
  number: 2248,
  url: "https://github.com/brightwave-inc/tidebreak/pull/2248",
  title: "Build the delivery center",
  state: "open",
  draft: false,
  author: "mara",
  author_avatar_url: "https://avatars.githubusercontent.com/u/1?v=4",
  head_branch: "thet/delivery-center",
  base_branch: "main",
  head_sha: "abc123",
  review_decision: "changes_requested",
  mergeable: "mergeable",
  merge_state_status: "blocked",
  auto_merge_enabled: false,
  checks: [
    {
      name: "desktop / storybook",
      bucket: "fail",
      detail: "completed",
      url: "https://github.com/brightwave-inc/tidebreak/actions/runs/77",
      workflow_run_id: 77,
    },
  ],
  attention_reasons: ["changes_requested", "checks_failed"],
  ready_to_merge: false,
  workspace_links: [DELIVERY_WORKSPACE_LINK],
  labels: ["desktop", "ui"],
  created_at: "2026-08-19T12:00:00.000Z",
  updated_at: "2026-08-20T12:00:00.000Z",
};

const DELIVERY_MERGED_PR = {
  ...DELIVERY_PR,
  id: "github.com/brightwave-inc/tidebreak#2240",
  number: 2240,
  url: "https://github.com/brightwave-inc/tidebreak/pull/2240",
  state: "merged",
  review_decision: undefined,
  attention_reasons: [],
  merged_at: "2026-08-19T16:02:00.000Z",
  closed_at: "2026-08-19T16:02:00.000Z",
};

const DELIVERY_RUN = {
  id: "github.com/brightwave-inc/tidebreak:workflow_run:77",
  repository: DELIVERY_REPOSITORY,
  kind: "workflow_run",
  github_id: 77,
  run_attempt: 2,
  name: "Desktop CI",
  url: "https://github.com/brightwave-inc/tidebreak/actions/runs/77",
  status: "completed",
  conclusion: "failure",
  workflow: "Desktop CI",
  branch: "thet/delivery-center",
  sha: "abc123",
  event: "pull_request",
  actor: "mara",
  attention_reasons: ["failure"],
  workspace_links: [DELIVERY_WORKSPACE_LINK],
  created_at: "2026-08-20T11:00:00.000Z",
  updated_at: "2026-08-20T12:05:00.000Z",
};

const SESSION = {
  id: "sess-1",
  workspace_id: "ws-1",
  kind: "interactive",
  harness_kind: "claude_code",
  permission_mode: "plan",
  lifecycle: "idle",
  attention: { state: { type: "working" }, source: "lifecycle" },
  unrecognized_event_count: 0,
  created_at: "2026-08-15T12:00:00.000Z",
};

describe("parseCodeSubscriptionUsage", () => {
  it("accepts normalized personal and shared provider windows", () => {
    const usage = {
      source: "model_gateway",
      diagnostics: [],
      providers: [
        {
          id: "anthropic",
          label: "Anthropic Direct",
          accounts: [
            {
              id: "personal",
              label: "Personal",
              is_own: true,
              state: "available",
              updated_at_unix_seconds: 1_776_000_000,
              windows: [
                {
                  key: "weekly",
                  label: "Weekly (7d)",
                  used_percent: 58,
                  resets_at_unix_seconds: 1_776_086_400,
                  status: "allowed",
                },
              ],
            },
          ],
        },
      ],
    };
    expect(parseCodeSubscriptionUsage(usage)).toEqual(usage);
  });

  it("rejects malformed usage percentages and sources", () => {
    const unavailable = {
      source: "unavailable",
      providers: [],
      diagnostics: ["No machine-readable usage source was found."],
    };
    expect(parseCodeSubscriptionUsage(unavailable)).toEqual(unavailable);
    expect(
      parseCodeSubscriptionUsage({ ...unavailable, source: "shell_scrape" }),
    ).toBeNull();
    expect(
      parseCodeSubscriptionUsage({
        source: "direct",
        diagnostics: [],
        providers: [
          {
            id: "openai",
            label: "Codex",
            accounts: [
              {
                id: "codex",
                label: "Codex Pro",
                is_own: true,
                state: "available",
                windows: [
                  {
                    key: "session",
                    label: "Session (5h)",
                    used_percent: "58",
                  },
                ],
              },
            ],
          },
        ],
      }),
    ).toBeNull();
  });
});

/**
 * Every state the server can send must parse.
 *
 * The parser is a runtime string switch, so a new variant on the Rust side is
 * invisible to `tsc` — and `parseCodeSessionList` returns null when any single
 * row fails, so one unparsed state blanks the whole session list rather than
 * degrading. `idle` shipped without landing here and did exactly that.
 *
 * Keep this list in step with `AttentionState` in `generated/wire.ts`.
 */
describe("parseCodeSession attention states", () => {
  const STATES = [
    { type: "working" },
    { type: "idle" },
    { type: "done_unreviewed" },
    { type: "needs_you", prompt: "approve this", source: "structured" },
    { type: "stalled", idle_secs: 42 },
    { type: "fenced", reason: { type: "orphan_alive" } },
    { type: "manual", note: "later" },
  ];

  it.each(STATES)("accepts %o", (state) => {
    const parsed = parseCodeSession({
      ...SESSION,
      attention: { state, source: "lifecycle" },
    });
    expect(parsed).not.toBeNull();
    expect(parsed?.attention.state.type).toBe(state.type);
  });

  it("still rejects a state the server cannot send", () => {
    expect(
      parseCodeSession({
        ...SESSION,
        attention: { state: { type: "napping" }, source: "lifecycle" },
      }),
    ).toBeNull();
  });
});

describe("parseCodeSessionList", () => {
  it("accepts GET /code/workspaces/{id}/sessions", () => {
    const ended = {
      ...SESSION,
      id: "sess-0",
      lifecycle: "ended",
    };
    expect(parseCodeSessionList([ended, SESSION])).toEqual([
      parseCodeSession(ended),
      parseCodeSession(SESSION),
    ]);
    expect(parseCodeSessionList([])).toEqual([]);
  });

  it("rejects a non-array or a row the session parser would drop", () => {
    expect(parseCodeSessionList({ sessions: [SESSION] })).toBeNull();
    expect(
      parseCodeSessionList([SESSION, { ...SESSION, lifecycle: "paused" }]),
    ).toBeNull();
  });
});

const TURN = {
  id: "turn-1",
  session_id: "sess-1",
  ordinal: 1,
  status: "completed",
  user_input: "list the files",
  attachments: [],
  started_at: "2026-08-15T12:00:00.000Z",
  ended_at: "2026-08-15T12:00:02.000Z",
};

const USAGE = {
  input_tokens: 12,
  output_tokens: 3,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
  context_tokens: 12,
};

describe("parseCodeTurnList", () => {
  it("accepts GET /code/sessions/{id}/turns", () => {
    expect(parseCodeTurnList([TURN])).toEqual([TURN]);
    expect(parseCodeTurnList([])).toEqual([]);
  });

  it("keeps optional usage on a completed turn", () => {
    const withUsage = { ...TURN, usage: USAGE };
    expect(parseCodeTurn(withUsage)).toEqual(withUsage);
    expect(parseCodeTurnList([withUsage])).toEqual([withUsage]);
  });

  it("reads a turn journaled before context_tokens existed as no reading", () => {
    // The field is serde-defaulted, so old rows arrive without it. Dropping
    // the whole usage object over a missing occupancy figure would lose the
    // spend counts beside it.
    const { context_tokens: _omitted, ...older } = USAGE;
    const parsed = parseCodeTurn({ ...TURN, usage: older });
    expect(parsed?.usage).toEqual({ ...older, context_tokens: 0 });
  });

  it("rejects a non-array or a row the turn parser would drop", () => {
    expect(parseCodeTurnList({ turns: [TURN] })).toBeNull();
    expect(parseCodeTurnList([{ ...TURN, status: "paused" }])).toBeNull();
  });
});

describe("parseCodeTurnSubmission", () => {
  it("tells a turn that ran from a follow-up the server queued", () => {
    // Both arrive as 202 on POST /code/sessions/{id}/turns. Reading the
    // queue receipt as a malformed turn reports a failure for a message the
    // server is holding, and the retry it invites double-sends.
    expect(parseCodeTurnSubmission(TURN)).toEqual({ kind: "ran", turn: TURN });
    const queued = {
      id: "q-1",
      session_id: "sess-1",
      message: "and run the tests",
      position: 0,
      created_at: "2026-08-24T00:00:00Z",
      updated_at: "2026-08-24T00:00:00Z",
    };
    expect(parseCodeTurnSubmission(queued)).toEqual({ kind: "queued", queued });
    expect(parseCodeTurnSubmission({ session_id: "sess-1" })).toBeNull();
  });
});

describe("parseCodeWorkspaceTree", () => {
  it("accepts GET /code/workspaces/{id}/tree", () => {
    const tree = { paths: ["README.md", "src/lib.rs"], truncated: false };
    expect(parseCodeWorkspaceTree(tree)).toEqual(tree);
  });

  it("rejects contents-shaped payloads", () => {
    expect(
      parseCodeWorkspaceTree({
        paths: ["README.md"],
        truncated: false,
        contents: "hello",
      }),
    ).toBeNull();
  });
});

describe("parseCodeWorkspaceSearch", () => {
  it("accepts bounded line matches and rejects malformed rows", () => {
    const result = {
      matches: [{ path: "src/lib.rs", line_number: 12, line: "fn crisp() {}" }],
      truncated: false,
    };
    expect(parseCodeWorkspaceSearch(result)).toEqual(result);
    expect(
      parseCodeWorkspaceSearch({
        ...result,
        matches: [{ ...result.matches[0], line_number: 0 }],
      }),
    ).toBeNull();
  });
});

describe("parseCodeWorkspaceFiles", () => {
  const files = {
    files: [
      {
        path: "src/lib.rs",
        kind: "modified",
        insertions: 3,
        deletions: 1,
      },
    ],
    truncated: false,
    stat: { files: 1, insertions: 3, deletions: 1, truncated: false },
    turn_id: "turn-1",
  };

  it("accepts GET /code/workspaces/{id}/files", () => {
    expect(parseCodeWorkspaceFiles(files)).toEqual(files);
  });

  it("rejects a missing stat or a bad kind", () => {
    expect(
      parseCodeWorkspaceFiles({ ...files, stat: { files: 1 } }),
    ).toBeNull();
    expect(
      parseCodeWorkspaceFiles({
        ...files,
        files: [{ ...files.files[0], kind: "moved" }],
      }),
    ).toBeNull();
  });
});

describe("parseCodeApproval", () => {
  it("accepts a pending approval snapshot", () => {
    const row = {
      id: "appr-1",
      session_id: "sess-1",
      turn_id: "turn-1",
      kind: { type: "file_write", paths: ["/workspace/probe.txt"] },
      harness_raw_json: '{"tool_name":"Write"}',
      state: "pending",
      requested_at: "2026-08-15T12:00:00.000Z",
    };
    expect(parseCodeApproval(row)).toEqual(row);
  });

  it("rejects a row without the harness payload", () => {
    expect(
      parseCodeApproval({
        id: "appr-1",
        session_id: "sess-1",
        turn_id: "turn-1",
        kind: { type: "other", summary: "x" },
        state: "pending",
        requested_at: "2026-08-15T12:00:00.000Z",
      }),
    ).toBeNull();
  });
});

describe("parseCodeWorkspaceBlob", () => {
  it("accepts GET /code/workspaces/{id}/blob", () => {
    const blob = {
      path: "src/lib.rs",
      content: "fn main() {}",
      truncated: false,
      binary: false,
    };
    expect(parseCodeWorkspaceBlob(blob)).toEqual(blob);
    expect(parseCodeWorkspaceBlob({ ...blob, extra: true })).toBeNull();
  });
});

describe("parseCodeWorkspaceDiff", () => {
  const diff = {
    diff: "--- a\n+++ b\n",
    truncated: true,
    stat: { files: 1, insertions: 1, deletions: 1, truncated: true },
    file: "src/lib.rs",
  };

  it("accepts GET /code/workspaces/{id}/diff", () => {
    expect(parseCodeWorkspaceDiff(diff)).toEqual(diff);
  });

  it("rejects a non-string body", () => {
    expect(parseCodeWorkspaceDiff({ ...diff, diff: 1 })).toBeNull();
  });
});

describe("parseCodeTerminal", () => {
  const terminal = {
    id: "term-1",
    workspace_id: "ws-1",
    cols: 80,
    rows: 24,
    ended: false,
    created_at: "2026-08-15T12:00:00.000Z",
  };

  it("accepts a live snapshot and a list", () => {
    expect(parseCodeTerminal(terminal)).toEqual(terminal);
    expect(parseCodeTerminalList([terminal])).toEqual([terminal]);
  });

  it("accepts a cursor-pull page", () => {
    const page = {
      id: "term-1",
      workspace_id: "ws-1",
      bytes: "aGVsbG8=",
      cursor: 5,
      overflow: false,
      truncated: false,
      ended: false,
    };
    expect(parseCodeTerminalRead(page)).toEqual(page);
  });
});

describe("parseCodeWorkspacePr", () => {
  const pr = {
    dirty: false,
    unpushed: true,
    ahead: 2,
    has_upstream: false,
    suggested_commit_message:
      "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)",
    gh_found: true,
    gh_authenticated: true,
    remediation: "",
    pr: {
      number: 12,
      url: "https://github.com/example/demo/pull/12",
      state: "open",
      checks_summary: "1 passing, 0 pending, 0 failing, 1 skipped",
      checks: [
        { name: "ci / rust", bucket: "pass" },
        { name: "release draft", bucket: "skipped", detail: "skipping" },
      ],
      draft: false,
      merged: false,
      review_decision: "changes_requested",
      mergeable: "mergeable",
      merge_state_status: "blocked",
      head_branch: "tidebreak/first-change",
      base_branch: "main",
      auto_merge_enabled: true,
      in_merge_queue: true,
    },
  };

  it("accepts GET /code/workspaces/{id}/pr", () => {
    expect(parseCodeWorkspacePr(pr)).toEqual(pr);
  });

  it("accepts a digest without the merge-status fields", () => {
    const sparse = {
      ...pr,
      pr: { number: 12, state: "open" },
    };
    expect(parseCodeWorkspacePr(sparse)).toEqual(sparse);
  });

  it("accepts the gh-absent remediation state", () => {
    const absent = {
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: false,
      suggested_commit_message:
        "Update workspace\n\n0 files changed, 0 insertions(+), 0 deletions(-)",
      gh_found: false,
      remediation: "gh is not installed. Install the GitHub CLI.",
    };
    expect(parseCodeWorkspacePr(absent)).toEqual(absent);
  });

  it("rejects a missing boolean", () => {
    expect(parseCodeWorkspacePr({ ...pr, dirty: "yes" })).toBeNull();
  });

  it("rejects a mistyped merge-status field", () => {
    expect(
      parseCodeWorkspacePr({ ...pr, pr: { ...pr.pr, draft: "yes" } }),
    ).toBeNull();
  });
});

describe("pull request state in live updates", () => {
  const richPr = {
    number: 12,
    url: "https://github.com/example/demo/pull/12",
    state: "open",
    title: "Keep the exact host state",
    checks_summary: "1 passing, 1 pending, 0 failing, 0 skipped",
    checks: [
      { name: "ci / rust", bucket: "pass" },
      {
        name: "ci / ui",
        bucket: "pending",
        detail: "running",
        url: "https://github.com/example/demo/actions/runs/1",
      },
    ],
    draft: true,
    merged: false,
    review_decision: "changes_requested",
    mergeable: "conflicting",
    merge_state_status: "behind",
    head_branch: "tidebreak/exact-pr-state",
    base_branch: "main",
    auto_merge_enabled: true,
    in_merge_queue: true,
  };

  const digest = {
    workspace: "ws-1",
    session: "sess-1",
    kind: "interactive",
    lifecycle: "idle",
    attention: { state: { type: "working" }, source: "lifecycle" },
    title: "Fix login",
    turn_count: 3,
    activity: "shell",
    pr_state: richPr,
  };

  it("preserves the complete PR digest in a digest notice", () => {
    expect(parseCodeUpdateNotice({ type: "digest", ...digest })).toEqual({
      type: "digest",
      ...digest,
    });
  });

  it("preserves the complete PR digest in a snapshot notice", () => {
    expect(
      parseCodeUpdateNotice({ type: "snapshot", sessions: [digest] }),
    ).toEqual({ type: "snapshot", sessions: [digest] });
  });

  it("rejects malformed rich PR fields instead of silently dropping them", () => {
    expect(
      parseCodeUpdateNotice({
        type: "digest",
        ...digest,
        pr_state: { ...richPr, in_merge_queue: "yes" },
      }),
    ).toBeNull();
  });

  it("rejects an unknown running activity instead of relabeling it", () => {
    expect(
      parseCodeUpdateNotice({
        type: "digest",
        ...digest,
        activity: "agents",
      }),
    ).toBeNull();
  });
});

describe("parseCodePrComments", () => {
  it("accepts GET /code/workspaces/{id}/pr/comments", () => {
    const comments = {
      number: 12,
      comments: [
        {
          kind: "issue",
          author: "alice",
          created_at: "2026-08-16T10:00:00Z",
          body: "looks close",
        },
        {
          kind: "review",
          author: "bob",
          created_at: "2026-08-16T11:00:00Z",
          body: "please split this",
          review_state: "changes_requested",
        },
        {
          kind: "inline",
          author: "bob",
          created_at: "2026-08-16T12:00:00Z",
          body: "rename this",
          path: "src/lib.rs",
          line: 42,
        },
      ],
    };
    expect(parseCodePrComments(comments)).toEqual(comments);
    expect(parseCodePrComments({ number: 12, comments: [] })).toEqual({
      number: 12,
      comments: [],
    });
  });

  it("rejects an unknown kind or a mistyped line", () => {
    const one = (comment: object) => ({ number: 12, comments: [comment] });
    expect(parseCodePrComments(one({ kind: "thread", body: "hi" }))).toBeNull();
    expect(
      parseCodePrComments(
        one({ kind: "inline", body: "hi", line: "forty-two" }),
      ),
    ).toBeNull();
  });
});

describe("parseCodeCommit", () => {
  it("accepts POST /code/workspaces/{id}/git/commit", () => {
    const commit = {
      sha: "abc123",
      message: "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)",
      stat: { files: 1, insertions: 1, deletions: 0, truncated: false },
    };
    expect(parseCodeCommit(commit)).toEqual(commit);
    expect(parseCodeCommit({ ...commit, sha: "" })).toBeNull();
  });
});

describe("parseCodePush", () => {
  it("accepts POST /code/workspaces/{id}/git/push", () => {
    expect(
      parseCodePush({ branch: "tidebreak/first", remote: "origin" }),
    ).toEqual({
      branch: "tidebreak/first",
      remote: "origin",
    });
  });
});

describe("parseCodeAction", () => {
  it("accepts POST /code/workspaces/{id}/actions/{name}", () => {
    const action = {
      name: "echo",
      success: true,
      exit_code: 0,
      stdout: "hello",
      stderr: "",
      timed_out: false,
    };
    expect(parseCodeAction(action)).toEqual(action);
  });
});

describe("parseCodeEvent", () => {
  it("takes tool_completed with or without the late-argument detail", () => {
    const completed = {
      type: "tool_completed",
      call_id: "toolu_1",
      outcome: "succeeded",
      preview: "ok",
    };
    // The correction is optional: adapters that never see the final
    // arguments omit it, and dropping the whole event over the new key
    // would stop every tool line from resolving.
    expect(parseCodeEvent(completed)).toEqual(completed);
    const corrected = {
      ...completed,
      detail: { kind: "command", cmd: "cargo test", cwd: "/workspace" },
    };
    expect(parseCodeEvent(corrected)).toEqual(corrected);
    expect(
      parseCodeEvent({ ...completed, detail: { kind: "command", cmd: 7 } }),
    ).toBeNull();
  });
});

describe("liveCodeSessions", () => {
  it("drops ended sessions and orders the rest oldest first", () => {
    const ended = parseCodeSession({
      ...SESSION,
      id: "old",
      lifecycle: "ended",
      created_at: "2026-08-15T09:00:00.000Z",
    });
    const later = parseCodeSession({
      ...SESSION,
      id: "sess-2",
      created_at: "2026-08-15T13:00:00.000Z",
    });
    const live = parseCodeSession(SESSION);
    expect(ended && later && live).toBeTruthy();
    // The list arrives newest first; the tab strip reads left to right in the
    // order the agents were started.
    expect(liveCodeSessions([later!, live!, ended!]).map((s) => s.id)).toEqual([
      "sess-1",
      "sess-2",
    ]);
    expect(liveCodeSessions([ended!])).toEqual([]);
  });

  it("never offers a watch session as a conversation", () => {
    const watch = parseCodeSession({
      ...SESSION,
      id: "watch-1",
      kind: "watch",
    });
    const live = parseCodeSession(SESSION);
    expect(watch && live).toBeTruthy();
    expect(liveCodeSessions([watch!, live!]).map((s) => s.id)).toEqual([
      "sess-1",
    ]);
    expect(liveCodeSessions([watch!])).toEqual([]);
  });
});

describe("worktree root parser", () => {
  it("keeps an absent root absent and refuses a wrong shape", () => {
    expect(
      parseCodeWorktreeRoot({
        effective_root: "/Users/sam/Tidebreak/workspaces",
        default_root: "/Users/sam/Tidebreak/workspaces",
      }),
    ).toEqual({
      effective_root: "/Users/sam/Tidebreak/workspaces",
      default_root: "/Users/sam/Tidebreak/workspaces",
    });
    expect(
      parseCodeWorktreeRoot({
        root: "/Volumes/work/trees",
        effective_root: "/Volumes/work/trees",
        default_root: "/Users/sam/Tidebreak/workspaces",
      }),
    ).toEqual({
      root: "/Volumes/work/trees",
      effective_root: "/Volumes/work/trees",
      default_root: "/Users/sam/Tidebreak/workspaces",
    });
    expect(parseCodeWorktreeRoot({ effective_root: "/tmp/trees" })).toBeNull();
    expect(
      parseCodeWorktreeRoot({
        root: 7,
        effective_root: "/tmp/trees",
        default_root: "/tmp/trees",
      }),
    ).toBeNull();
  });
});

describe("clone wire parsers", () => {
  it("accepts a clone job snapshot and defaults payload", () => {
    const job = {
      id: "job-1",
      phase: "receiving objects",
      percent: 40,
      done: false,
    };
    expect(parseCodeCloneJob(job)).toEqual(job);
    expect(
      parseCodeCloneDefaults({
        parent_dir: "/tmp/src",
        gh_found: false,
        gh_remediation: "gh is not installed.",
      }),
    ).toEqual({
      parent_dir: "/tmp/src",
      gh_found: false,
      gh_remediation: "gh is not installed.",
    });
  });

  it("accepts a clone_progress update notice", () => {
    expect(
      parseCodeUpdateNotice({
        type: "clone_progress",
        job: "job-1",
        phase: "done",
        percent: 100,
        done: true,
        repo_id: "repo-1",
      }),
    ).toEqual({
      type: "clone_progress",
      job: "job-1",
      phase: "done",
      percent: 100,
      done: true,
      repo_id: "repo-1",
    });
  });

  it("accepts a harness_install update notice and refuses an unknown engine", () => {
    expect(
      parseCodeUpdateNotice({
        type: "harness_install",
        kind: "claude_code",
        version: "2.1.234",
        phase: "installing",
        done: false,
      }),
    ).toEqual({
      type: "harness_install",
      kind: "claude_code",
      version: "2.1.234",
      phase: "installing",
      done: false,
    });
    expect(
      parseCodeUpdateNotice({
        type: "harness_install",
        kind: "not_an_engine",
        phase: "installing",
        done: false,
      }),
    ).toBeNull();
  });
});

describe("code delivery wire parsers", () => {
  it("accepts a repositories snapshot with partial source errors", () => {
    const snapshot = {
      capability: DELIVERY_CAPABILITY,
      repositories: [DELIVERY_REPOSITORY],
      errors: [
        {
          repository: {
            host: "github.com",
            owner: "brightwave-inc",
            name: "private-repo",
          },
          kind: "forbidden",
          message: "Repository access is unavailable.",
        },
      ],
      fetched_at: "2026-08-20T12:10:00.000Z",
    };

    expect(parseCodeDeliveryRepositories(snapshot)).toEqual(snapshot);
  });

  it("accepts pull request pages and full details", () => {
    const page = {
      capability: DELIVERY_CAPABILITY,
      items: [DELIVERY_PR],
      next_cursor: "next-pr-page",
      errors: [],
      fetched_at: "2026-08-20T12:10:00.000Z",
    };
    const detail = {
      summary: DELIVERY_PR,
      body: "A cross-repository view of delivery state.",
      labels: ["desktop", "ui"],
      assignees: ["mara"],
      requested_reviewers: ["devon"],
      changed_files: 8,
      additions: 640,
      deletions: 91,
      commits: 4,
      merged_by: "devon",
      files: [
        {
          path: "src/code/CodeDeliveryPage.tsx",
          status: "modified",
          additions: 184,
          deletions: 61,
          patch: "@@ -1 +1 @@\n-old\n+new",
        },
        {
          path: "docs/assets/delivery.png",
          status: "added",
          additions: 0,
          deletions: 0,
        },
      ],
      files_truncated: false,
      comments: [
        {
          kind: "inline",
          id: "comment-1",
          author: "devon",
          url: "https://github.com/brightwave-inc/tidebreak/pull/2248#discussion_r1",
          created_at: "2026-08-20T12:07:00.000Z",
          body: "Keep this state visible at narrow widths.",
          path: "src/code/CodeDeliveryPage.tsx",
          line: 420,
        },
      ],
      errors: [
        {
          repository: {
            host: "github.com",
            owner: "brightwave-inc",
            name: "tidebreak",
          },
          kind: "transient",
          message: "Could not load reviews: GitHub did not answer in time.",
        },
      ],
      can_mark_ready: false,
      can_merge: false,
      can_rerun_failed: true,
      can_close: true,
      can_reopen: false,
      can_comment: true,
    };

    expect(parseCodeDeliveryPullRequestsPage(page)).toEqual(page);
    expect(parseCodeDeliveryPullRequestDetail(detail)).toEqual(detail);
  });

  it("carries the merge and close timestamps that settle a pull request", () => {
    const page = {
      capability: DELIVERY_CAPABILITY,
      items: [DELIVERY_MERGED_PR],
      errors: [],
      fetched_at: "2026-08-20T12:10:00.000Z",
    };

    const parsed = parseCodeDeliveryPullRequestsPage(page);
    expect(parsed?.items[0]?.merged_at).toBe("2026-08-19T16:02:00.000Z");
    expect(parsed?.items[0]?.closed_at).toBe("2026-08-19T16:02:00.000Z");
    expect(parsed?.items[0]?.state).toBe("merged");
  });

  it("accepts run pages and details for Actions and deployments", () => {
    const deployment = {
      ...DELIVERY_RUN,
      id: "github.com/brightwave-inc/tidebreak:deployment:91",
      kind: "deployment",
      github_id: 91,
      name: "Production",
      url: "https://github.com/brightwave-inc/tidebreak/deployments/activity_log?environment=production",
      status: "failure",
      environment: "production",
      attention_reasons: ["failure"],
    };
    const page = {
      capability: DELIVERY_CAPABILITY,
      items: [DELIVERY_RUN, deployment],
      errors: [],
      fetched_at: "2026-08-20T12:10:00.000Z",
    };
    const detail = {
      summary: DELIVERY_RUN,
      jobs: [
        {
          id: 801,
          name: "storybook",
          status: "completed",
          conclusion: "failure",
          url: "https://github.com/brightwave-inc/tidebreak/actions/runs/77/job/801",
          started_at: "2026-08-20T11:02:00.000Z",
          completed_at: "2026-08-20T11:08:00.000Z",
          failed_steps: ["Build Storybook"],
        },
      ],
      deployment_statuses: [
        {
          id: 901,
          state: "failure",
          description: "Production health check failed.",
          environment_url: "https://tidebreak.example.com",
          log_url:
            "https://github.com/brightwave-inc/tidebreak/actions/runs/77",
          created_at: "2026-08-20T12:04:00.000Z",
        },
      ],
      errors: [
        {
          repository: {
            host: "github.com",
            owner: "brightwave-inc",
            name: "tidebreak",
          },
          kind: "truncated",
          message: "Jobs may be incomplete.",
        },
      ],
      can_rerun_failed: true,
    };

    expect(parseCodeDeliveryRunsPage(page)).toEqual(page);
    expect(parseCodeDeliveryRunDetail(detail)).toEqual(detail);
  });

  it("rejects malformed nested delivery rows instead of dropping them", () => {
    expect(
      parseCodeDeliveryPullRequestsPage({
        capability: DELIVERY_CAPABILITY,
        items: [
          {
            ...DELIVERY_PR,
            checks: [{ ...DELIVERY_PR.checks[0], bucket: "flaky" }],
          },
        ],
        errors: [],
        fetched_at: "2026-08-20T12:10:00.000Z",
      }),
    ).toBeNull();
    expect(
      parseCodeDeliveryRunDetail({
        summary: DELIVERY_RUN,
        jobs: [
          {
            id: 801,
            name: "storybook",
            status: "completed",
            url: "https://github.com/brightwave-inc/tidebreak/actions/runs/77/job/801",
            started_at: null,
            completed_at: null,
            failed_steps: [7],
          },
        ],
        deployment_statuses: [],
        errors: [],
        can_rerun_failed: true,
      }),
    ).toBeNull();
  });

  it("accepts a bounded action result", () => {
    expect(
      parseCodeDeliveryActionResult({
        success: false,
        message: "One workflow run failed.",
        rerun_outcomes: [
          { workflow_run_id: 10, success: true },
          { workflow_run_id: 11, success: false, error: "HTTP 503" },
        ],
      }),
    ).toEqual({
      success: false,
      message: "One workflow run failed.",
      rerun_outcomes: [
        { workflow_run_id: 10, success: true },
        { workflow_run_id: 11, success: false, error: "HTTP 503" },
      ],
    });
    expect(
      parseCodeDeliveryActionResult({ success: "yes", message: "done" }),
    ).toBeNull();
    expect(
      parseCodeDeliveryActionResult({
        success: false,
        message: "failed",
        rerun_outcomes: [{ workflow_run_id: 0, success: false }],
      }),
    ).toBeNull();
  });
});

describe("parseFenceReason", () => {
  it("accepts every reason the server can send", () => {
    expect(parseFenceReason({ type: "orphan_alive" })).toEqual({
      type: "orphan_alive",
    });
    expect(parseFenceReason({ type: "resume_lost", detail: "gone" })).toEqual({
      type: "resume_lost",
      detail: "gone",
    });
    // A session fenced for repeated failures used to fail this parse, and
    // a null here drops the whole session from the list rather than just
    // its badge.
    expect(
      parseFenceReason({
        type: "repeated_turn_failures",
        count: 3,
        detail: "401",
      }),
    ).toEqual({ type: "repeated_turn_failures", count: 3, detail: "401" });
  });

  it("rejects a malformed reason", () => {
    expect(parseFenceReason({ type: "repeated_turn_failures" })).toBeNull();
    expect(
      parseFenceReason({
        type: "repeated_turn_failures",
        count: "3",
        detail: "x",
      }),
    ).toBeNull();
    expect(parseFenceReason({ type: "who_knows" })).toBeNull();
  });
});

describe("pull request facts (decision 62)", () => {
  const fact = {
    host: "github.com",
    repo_owner: "acme",
    repo_name: "tools",
    number: 412,
    url: "https://github.com/acme/tools/pull/412",
    title: "Add fact tracking",
    state: "open",
    draft: false,
    author: "octocat",
    head_branch: "feat/x",
    base_branch: "main",
    head_sha: "aaa111",
    relation: "authored",
    created_at: "2026-08-22T10:00:00Z",
    updated_at: "2026-08-22T11:00:00Z",
    last_seen_at: "2026-08-22T11:00:30Z",
  };

  it("accepts GET /code/workspaces/{id}/pull-requests", () => {
    const page = { items: [fact], fetched_at: "2026-08-22T11:00:31Z" };
    expect(parseCodeWorkspacePullRequests(page)).toEqual(page);
  });

  it("rejects an unknown relation or state instead of guessing", () => {
    expect(
      parseCodeWorkspacePullRequests({
        items: [{ ...fact, relation: "reviewed" }],
        fetched_at: "2026-08-22T11:00:31Z",
      }),
    ).toBeNull();
    expect(
      parseCodeWorkspacePullRequests({
        items: [{ ...fact, state: "OPEN" }],
        fetched_at: "2026-08-22T11:00:31Z",
      }),
    ).toBeNull();
  });

  it("carries pr_count through a digest notice", () => {
    const digest = {
      workspace: "ws-1",
      session: "sess-1",
      kind: "interactive",
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: "Fix login",
      turn_count: 3,
      pr_count: 3,
    };
    expect(parseCodeUpdateNotice({ type: "digest", ...digest })).toEqual({
      type: "digest",
      ...digest,
    });
    expect(
      parseCodeUpdateNotice({ type: "digest", ...digest, pr_count: "3" }),
    ).toBeNull();
  });

  it("carries a durable relation on a delivery workspace link", () => {
    const page = {
      capability: {
        found: true,
        authenticated: true,
        remediation: "",
      },
      items: [
        {
          id: "github.com/acme/tools#412",
          repository: {
            host: "github.com",
            owner: "acme",
            name: "tools",
            name_with_owner: "acme/tools",
            url: "https://github.com/acme/tools",
          },
          number: 412,
          url: "https://github.com/acme/tools/pull/412",
          title: "Add fact tracking",
          state: "open",
          draft: false,
          head_branch: "feat/x",
          base_branch: "main",
          auto_merge_enabled: false,
          checks: [],
          attention_reasons: [],
          ready_to_merge: false,
          workspace_links: [
            {
              workspace_id: "ws-1",
              repo_id: "repo-1",
              title: "facts",
              branch_name: "feat/x",
              status: "active",
              exact: true,
              relation: "contributed",
            },
          ],
          stack_parent_number: 400,
          labels: [],
          created_at: "2026-08-22T10:00:00Z",
          updated_at: "2026-08-22T11:00:00Z",
        },
      ],
      errors: [],
      fetched_at: "2026-08-22T11:00:31Z",
    };
    expect(parseCodeDeliveryPullRequestsPage(page)).toEqual(page);
    expect(
      parseCodeDeliveryPullRequestsPage({
        ...page,
        items: [
          {
            ...page.items[0],
            workspace_links: [
              { ...page.items[0].workspace_links[0], relation: "owner" },
            ],
          },
        ],
      }),
    ).toBeNull();
  });
});

describe("parseCodeCheckLogsSnapshot", () => {
  it("accepts a snapshot with and without the optional fields", () => {
    const full = {
      head_sha: "abc123",
      logs: [
        {
          check: "clippy",
          path: "/data/code/private/ws-1/ci-logs/clippy-1.log",
          byte_len: 2048,
          truncated: true,
          url: "https://github.com/acme/app/actions/runs/7/job/9",
        },
      ],
      errors: [{ check: "desktop UI", message: "HTTP 404" }],
    };
    expect(parseCodeCheckLogsSnapshot(full)).toEqual(full);
    const bare = { logs: [], errors: [] };
    expect(parseCodeCheckLogsSnapshot(bare)).toEqual(bare);
  });

  it("rejects a malformed log entry rather than dropping it", () => {
    expect(
      parseCodeCheckLogsSnapshot({
        logs: [{ check: "clippy", path: "", byte_len: 1, truncated: false }],
        errors: [],
      }),
    ).toBeNull();
    expect(
      parseCodeCheckLogsSnapshot({ logs: [], errors: [], extra: true }),
    ).toBeNull();
  });
});
