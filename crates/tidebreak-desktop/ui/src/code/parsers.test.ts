import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import {
  liveCodeSessions,
  parseCodeAction,
  parseCodeAnalytics,
  parseCodeApproval,
  parseCodeCloneDefaults,
  parseCodeCloneJob,
  parseCodeCommit,
  parseCodeGrant,
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
  parseHarnessModelList,
  parseCodeCheckLogsSnapshot,
  parseCodeRepo,
  parseCodeSessionDigest,
  parseCodeWorkspace,
  parseHarnessDoctorReport,
  parseQueuedCodeTurn,
  parseSequencedCodeEvent,
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
  in_merge_queue: false,
  comment_count: 3,
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

describe("parseHarnessModelList", () => {
  const listing = {
    kind: "opencode",
    models: [
      {
        id: "model-gateway/glm-5.3",
        label: "GLM 5.3",
        default: true,
        reasoning_efforts: [],
        fast_mode: false,
      },
    ],
    reasoning_efforts: [],
  };

  it("preserves hosted catalog provenance and defaults old servers to native", () => {
    expect(
      parseHarnessModelList({ ...listing, source: "model_gateway" }),
    ).toEqual({ ...listing, source: "model_gateway" });
    expect(parseHarnessModelList(listing)).toEqual({
      ...listing,
      source: "harness",
    });
  });

  it("rejects an unknown catalog source", () => {
    expect(parseHarnessModelList({ ...listing, source: "shared" })).toBeNull();
  });
});

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

describe("parseCodeAnalytics", () => {
  const snapshot = {
    range: "30d",
    from: "2026-07-26T16:00:00.000Z",
    through: "2026-08-24T16:00:00.000Z",
    totals: {
      sessions: 3,
      turns: 8,
      completed_turns: 6,
      failed_turns: 1,
      interrupted_turns: 1,
      running_turns: 0,
      input_tokens: 1_000,
      output_tokens: 500,
      cache_read_tokens: 2_000,
      cache_write_tokens: 100,
      total_tokens: 3_600,
      estimated_cost_microusd: 12_300,
      pull_requests_opened: 2,
      pull_requests_merged: 1,
    },
    daily: [
      {
        date: "2026-08-24",
        sessions: 3,
        turns: 8,
        total_tokens: 3_600,
        estimated_cost_microusd: 12_300,
        pull_requests_opened: 2,
        pull_requests_merged: 1,
      },
    ],
    repositories: [
      {
        repo_id: "repo-1",
        name: "tidebreak",
        sessions: 3,
        turns: 8,
        total_tokens: 3_600,
        estimated_cost_microusd: 12_300,
        pull_requests_opened: 2,
        pull_requests_merged: 1,
      },
    ],
    models: [
      {
        model_id: "claude-sonnet-5",
        harness_kind: "claude_code",
        fast_mode: false,
        sessions: 3,
        turns: 8,
        total_tokens: 3_600,
        estimated_cost_microusd: 12_300,
        priced: true,
      },
    ],
    harnesses: [
      {
        harness_kind: "claude_code",
        sessions: 3,
        turns: 8,
        total_tokens: 3_600,
        estimated_cost_microusd: 12_300,
      },
    ],
    pricing: {
      priced_turns: 8,
      unpriced_turns: 0,
      priced_tokens: 3_600,
      unpriced_tokens: 0,
      prices_as_of: "2026-08-21",
    },
  };

  it("accepts the complete analytics contract", () => {
    expect(parseCodeAnalytics(snapshot)).toEqual(snapshot);
  });

  it("rejects unknown ranges and malformed totals", () => {
    expect(parseCodeAnalytics({ ...snapshot, range: "month" })).toBeNull();
    expect(
      parseCodeAnalytics({
        ...snapshot,
        totals: { ...snapshot.totals, total_tokens: -1 },
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

describe("parseCodeSession without a workspace", () => {
  // The in-process engine's session binds no workspace (decision 0048
  // step 5), so the server sends `workspace_id: null`; the key is present.
  it("keeps the null workspace instead of dropping the session", () => {
    const parsed = parseCodeSession({ ...SESSION, workspace_id: null });
    expect(parsed).not.toBeNull();
    expect(parsed?.workspace_id).toBeNull();
  });

  it("accepts the in-process engine's harness kind", () => {
    // The server fixture names `internal`, and the parser's own vocabulary
    // had not caught up, so the session dropped for that reason too.
    const parsed = parseCodeSession({
      ...SESSION,
      workspace_id: null,
      harness_kind: "internal",
    });
    expect(parsed?.harness_kind).toBe("internal");
  });

  it("still requires the key and a real id when one is given", () => {
    const { workspace_id: _omitted, ...missing } = SESSION;
    expect(parseCodeSession(missing)).toBeNull();
    expect(parseCodeSession({ ...SESSION, workspace_id: "" })).toBeNull();
  });
});

describe("parseCodeSession external origin", () => {
  it("carries a well-formed origin through", () => {
    const parsed = parseCodeSession({
      ...SESSION,
      external_origin: {
        channel_kind: "slack",
        external_key: "T04/C08/1724900000.123456",
      },
    });
    expect(parsed?.external_origin).toEqual({
      channel_kind: "slack",
      external_key: "T04/C08/1724900000.123456",
    });
  });

  it("rejects an origin with unknown keys or empty parts", () => {
    expect(
      parseCodeSession({
        ...SESSION,
        external_origin: { channel_kind: "slack", external_key: "" },
      }),
    ).toBeNull();
    expect(
      parseCodeSession({
        ...SESSION,
        external_origin: {
          channel_kind: "slack",
          external_key: "T04/C08/1.2",
          extra: true,
        },
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
  fast_mode: false,
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
  it("accepts GET /sessions/{id}/turns", () => {
    expect(parseCodeTurnList([TURN])).toEqual([TURN]);
    expect(parseCodeTurnList([])).toEqual([]);
  });

  it("keeps optional usage on a completed turn", () => {
    const withUsage = { ...TURN, usage: USAGE };
    expect(parseCodeTurn(withUsage)).toEqual(withUsage);
    expect(parseCodeTurnList([withUsage])).toEqual([withUsage]);
  });

  it("keeps the execution settings snapshotted for the turn", () => {
    const selected = { ...TURN, model: "steady", fast_mode: true };
    expect(parseCodeTurn(selected)).toEqual(selected);
  });

  it("reads a turn journaled before context_tokens existed as no reading", () => {
    // The field is serde-defaulted, so old rows arrive without it. Dropping
    // the whole usage object over a missing occupancy figure would lose the
    // spend counts beside it.
    const { context_tokens: _omitted, ...older } = USAGE;
    const parsed = parseCodeTurn({ ...TURN, usage: older });
    expect(parsed?.usage).toEqual({ ...older, context_tokens: 0 });
  });

  it("keeps optional first-call context and accepts older usage without it", () => {
    expect(
      parseCodeTurn({
        ...TURN,
        usage: { ...USAGE, first_call_context_tokens: 9_500 },
      })?.usage,
    ).toEqual({ ...USAGE, first_call_context_tokens: 9_500 });
    expect(parseCodeTurn({ ...TURN, usage: USAGE })?.usage).toEqual(USAGE);
  });

  it("rejects a non-array or a row the turn parser would drop", () => {
    expect(parseCodeTurnList({ turns: [TURN] })).toBeNull();
    expect(parseCodeTurnList([{ ...TURN, status: "paused" }])).toBeNull();
  });
});

describe("parseCodeTurnSubmission", () => {
  it("tells a turn that ran from a follow-up the server queued", () => {
    // Both arrive as 202 on POST /sessions/{id}/turns. Reading the
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

  it("accepts history matches that address the producing session", () => {
    const result = {
      matches: [],
      history_matches: [
        {
          workspace_id: "workspace-1",
          workspace_title: "Archived search work",
          session_id: "session-1",
          turn_id: "turn-1",
          source: "turn_user_input",
          preview: "Find the archived transcript.",
          created_at: "2026-08-25T12:00:00Z",
        },
      ],
      truncated: false,
    };
    expect(parseCodeWorkspaceSearch(result)).toEqual(result);
    expect(
      parseCodeWorkspaceSearch({
        ...result,
        history_matches: [{ ...result.history_matches[0], session_id: "" }],
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

describe("parseCodeGrant", () => {
  const grant = {
    id: "grant-1",
    channel_kind: "slack",
    external_identity: "U04CASEY",
    display_name: "Casey Nakamura",
    workspace_identity: "T04ACME",
    workspace_name: "Acme Corp",
    avatar_url: "https://example.com/avatar.png",
    created_at: "2026-08-20T10:00:00.000Z",
  };

  it("keeps the identity details shown during connect", () => {
    expect(parseCodeGrant(grant)).toEqual(grant);
  });

  it("accepts grants minted before connect profiles existed", () => {
    const {
      display_name: _displayName,
      workspace_name: _workspaceName,
      avatar_url: _avatarUrl,
      ...older
    } = grant;
    expect(parseCodeGrant(older)).toEqual(older);
  });

  it("rejects malformed optional identity details", () => {
    expect(parseCodeGrant({ ...grant, avatar_url: 7 })).toBeNull();
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
      check_counts: { passing: 1, pending: 0, failing: 0, skipped: 1 },
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

  it("rejects mistyped check counts", () => {
    expect(
      parseCodeWorkspacePr({
        ...pr,
        pr: {
          ...pr.pr,
          check_counts: { passing: "1", pending: 0, failing: 0, skipped: 0 },
        },
      }),
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
    harness_kind: "claude_code",
    lifecycle: "idle",
    attention: { state: { type: "working" }, source: "lifecycle" },
    title: "Fix login",
    turn_count: 3,
    trigger_target_at: "2026-08-29T12:00:00Z",
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

  it("keeps a digest whose session binds no workspace", () => {
    const orphan = { ...digest, workspace: null };
    expect(parseCodeSessionDigest(orphan)).toEqual(orphan);
    expect(parseCodeUpdateNotice({ type: "digest", ...orphan })).toEqual({
      type: "digest",
      ...orphan,
    });
    expect(
      parseCodeUpdateNotice({ type: "snapshot", sessions: [orphan] }),
    ).toEqual({ type: "snapshot", sessions: [orphan] });
  });

  it("still rejects a digest with the workspace key missing or empty", () => {
    const { workspace: _omitted, ...missing } = digest;
    expect(parseCodeSessionDigest(missing)).toBeNull();
    expect(parseCodeUpdateNotice({ type: "digest", ...missing })).toBeNull();
    expect(
      parseCodeUpdateNotice({ type: "digest", ...digest, workspace: "" }),
    ).toBeNull();
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

  it("rejects an unknown digest harness instead of showing the wrong brand", () => {
    expect(
      parseCodeUpdateNotice({
        type: "digest",
        ...digest,
        harness_kind: "cursor",
      }),
    ).toBeNull();
  });

  it("rejects a non-string trigger target timestamp", () => {
    expect(
      parseCodeUpdateNotice({
        type: "digest",
        ...digest,
        trigger_target_at: 42,
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
  it("keeps the internal engine's rows on the code wire", () => {
    // A chat is a session on the one journal, so its rows carry fields an
    // external engine never writes. Dropping them would leave every tool
    // completion and turn terminal of an internal session unread.
    const usage = {
      input_tokens: 3,
      output_tokens: 4,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
      context_tokens: 0,
    };
    const completed = {
      type: "tool_completed",
      call_id: "toolu_1",
      outcome: "succeeded",
      preview: "ok",
      output: { content: "ok", is_error: false },
      action: { tool: "exec", command: "echo", args: [], cwd: ".", files: [] },
      result: null,
    };
    expect(parseCodeEvent(completed)).toEqual({
      type: "tool_completed",
      call_id: "toolu_1",
      outcome: "succeeded",
      preview: "ok",
    });
    expect(
      parseCodeEvent({
        type: "turn_completed",
        usage,
        stop_reason: "end_turn",
      }),
    ).toEqual({ type: "turn_completed", usage });
    expect(
      parseCodeEvent({
        type: "turn_failed",
        error: { message: "config: no provider" },
        detail: { kind: "config", message: "no provider" },
      }),
    ).toEqual({
      type: "turn_failed",
      error: { message: "config: no provider" },
    });
    expect(parseCodeEvent({ type: "turn_interrupted", usage })).toEqual({
      type: "turn_interrupted",
      usage,
    });
    expect(
      parseCodeEvent({
        type: "turn_resumed",
        turn_id: "11111111-1111-4111-8111-111111111111",
      }),
    ).toEqual({
      type: "turn_resumed",
      turn_id: "11111111-1111-4111-8111-111111111111",
    });
    expect(
      parseCodeEvent({
        type: "user_steered",
        text: "and say thanks",
        message_id: "11111111-1111-4111-8111-111111111111",
      }),
    ).toEqual({
      type: "user_steered",
      text: "and say thanks",
      message_id: "11111111-1111-4111-8111-111111111111",
    });
    const refused = {
      type: "turn_refused",
      usage,
      refusal: {
        details: { category: "blocked" },
        partial_output: true,
        source: "report_blocked",
      },
    };
    expect(parseCodeEvent(refused)).toEqual(refused);
    expect(
      parseCodeEvent({
        ...refused,
        refusal: { ...refused.refusal, source: "provider" },
      }),
    ).toBeNull();
    expect(
      parseCodeEvent({
        ...refused,
        refusal: { details: {}, partial_output: 1 },
      }),
    ).toBeNull();
  });

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

  it("takes structured approval resolutions alongside the plain ones", () => {
    for (const decision of [
      { type: "approved_with_grant", scope: { tool: "export", rung: "turn" } },
      { type: "answered", answers: [{ id: "q1", choice: "us-east" }] },
      { type: "plan_decided", approve: false, feedback: "Split the plan." },
    ]) {
      const resolved = {
        type: "approval_resolved",
        approval_id: "appr-1",
        decision,
      };
      expect(parseCodeEvent(resolved)).toEqual(resolved);
    }
    expect(
      parseCodeEvent({
        type: "approval_resolved",
        approval_id: "appr-1",
        decision: { type: "escalated" },
      }),
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
    // The remote-session causes (issue 2870): a null would drop a fenced
    // remote session from the list rather than just its badge.
    expect(
      parseFenceReason({
        type: "incarnation_unresolved",
        detail: "no spawn outcome",
      }),
    ).toEqual({ type: "incarnation_unresolved", detail: "no spawn outcome" });
    expect(
      parseFenceReason({ type: "sandbox_lost", detail: "node loss" }),
    ).toEqual({
      type: "sandbox_lost",
      detail: "node loss",
    });
    expect(
      parseFenceReason({
        type: "terminal_flush_missing",
        detail: "events unread",
      }),
    ).toEqual({ type: "terminal_flush_missing", detail: "events unread" });
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
    expect(parseFenceReason({ type: "sandbox_lost" })).toBeNull();
    expect(parseFenceReason({ type: "who_knows" })).toBeNull();
  });
});

describe("pull request facts (decision 77)", () => {
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

  it("carries memory_proposal_count through a digest notice", () => {
    const digest = {
      workspace: "ws-1",
      session: "sess-1",
      kind: "interactive",
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: "Fix login",
      turn_count: 3,
      memory_proposal_count: 2,
    };
    expect(parseCodeUpdateNotice({ type: "digest", ...digest })).toEqual({
      type: "digest",
      ...digest,
    });
    expect(parseCodeSessionDigest(digest)).toEqual(digest);
    expect(
      parseCodeUpdateNotice({
        type: "digest",
        ...digest,
        memory_proposal_count: "2",
      }),
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
          unregistered_stack_numbers: [400, 412],
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

describe("string bounds", () => {
  const page = (pr: object) => ({
    capability: DELIVERY_CAPABILITY,
    items: [pr],
    errors: [],
    fetched_at: "2026-08-20T12:10:00.000Z",
  });
  const oneComment = (body: string) => ({
    number: 12,
    comments: [
      {
        kind: "issue",
        author: "alice",
        created_at: "2026-08-16T10:00:00Z",
        body,
      },
    ],
  });

  it("keeps a one-line field at its limit and drops one past it", () => {
    const atLimit = "t".repeat(4_096);
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, title: atLimit }),
      )?.items[0]?.title,
    ).toBe(atLimit);
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, title: `${atLimit}!` }),
      ),
    ).toBeNull();
  });

  it("shares the id and timestamp limits with the chat decoder", () => {
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, id: "i".repeat(128) }),
      ),
    ).not.toBeNull();
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, id: "i".repeat(129) }),
      ),
    ).toBeNull();
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, created_at: "2".repeat(65) }),
      ),
    ).toBeNull();
    expect(parseCodeTurn({ ...TURN, id: " " })).toBeNull();
  });

  it("rejects a control or bidirectional character on a one-line field", () => {
    for (const title of [
      "fix: \u001b[31mred\u001b[0m",
      "fix: \u202eevil",
      "fix: two\nlines",
    ]) {
      expect(
        parseCodeDeliveryPullRequestsPage(page({ ...DELIVERY_PR, title })),
      ).toBeNull();
    }
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, head_branch: "feat/bell\u0007" }),
      ),
    ).toBeNull();
    expect(
      parseCodeDeliveryPullRequestsPage(
        page({ ...DELIVERY_PR, labels: ["ok", "bad\u2066"] }),
      ),
    ).toBeNull();
  });

  it("keeps line breaks, CRLF, and tabs in a block field but nothing else", () => {
    const crlf = "## Summary\r\n\r\n- one\r\n\t- nested\n";
    expect(parseCodePrComments(oneComment(crlf))?.comments[0]?.body).toBe(crlf);
    expect(parseCodePrComments(oneComment("a\u001b[2Jb"))).toBeNull();
    expect(parseCodePrComments(oneComment("a\u202eb"))).toBeNull();
    expect(parseCodePrComments(oneComment("x".repeat(1_048_577)))).toBeNull();
    expect(
      parseCodeEvent({ type: "assistant_delta", text: "line\r\nnext" }),
    ).toEqual({ type: "assistant_delta", text: "line\r\nnext" });
    expect(
      parseCodeEvent({ type: "assistant_delta", text: "\u001b[1mbold" }),
    ).toBeNull();
  });

  it("bounds verbatim payloads by length only", () => {
    const content = "progress\r\u001b[32mdone\u001b[0m\u202e\n";
    const blob = { path: "out.log", content, truncated: false, binary: false };
    expect(parseCodeWorkspaceBlob(blob)).toEqual(blob);
    expect(
      parseCodeWorkspaceBlob({ ...blob, content: "x".repeat(4_194_305) }),
    ).toBeNull();
    expect(
      parseCodeWorkspaceBlob({ ...blob, path: "out\u0007.log" }),
    ).toBeNull();
    const diff = {
      diff: "--- a\n+++ b\n-\u001b[0m\n",
      truncated: false,
      stat: { files: 1, insertions: 0, deletions: 1, truncated: false },
    };
    expect(parseCodeWorkspaceDiff(diff)).toEqual(diff);
    const preview = {
      type: "tool_completed",
      call_id: "toolu_1",
      outcome: "succeeded",
      preview: "\u001b[32m✓\u001b[0m 12 passed",
    };
    expect(parseCodeEvent(preview)).toEqual(preview);
  });

  it("never drops a turn over the user's own pasted text", () => {
    const pasted = { ...TURN, user_input: "why does this print \u001b[31m?" };
    expect(parseCodeTurn(pasted)).toEqual(pasted);
    const steer = { type: "user_steered", text: "stop; it prints \u001b[31m" };
    expect(parseCodeEvent(steer)).toEqual(steer);
    // A history excerpt is cut from that same text, so it is raw as well.
    const search = {
      matches: [],
      history_matches: [
        {
          workspace_id: "workspace-1",
          workspace_title: "Colored test run",
          session_id: "session-1",
          source: "event",
          preview: "\u001b[32m\u2713\u001b[0m 12 passed",
          created_at: "2026-08-25T12:00:00Z",
        },
      ],
      truncated: false,
    };
    expect(parseCodeWorkspaceSearch(search)).toEqual(search);
  });
});

/**
 * One real value of every snapshot, update notice, and event frame the code
 * surface serializes, written by the server's own types. The CLI's tests and
 * the server's round trip read the same file, so the three decoders of this
 * surface cannot drift apart without one of them failing.
 */
const CODE_FRAMES: { name: string; kind: string; value: unknown }[] =
  JSON.parse(
    readFileSync(
      fileURLToPath(
        new URL(
          "../../../../tidebreak-server-api/fixtures/code-frames.json",
          import.meta.url,
        ),
      ),
      "utf8",
    ),
  );

const CODE_FRAME_PARSERS: Record<string, (value: unknown) => unknown> = {
  repo: parseCodeRepo,
  workspace: parseCodeWorkspace,
  session: parseCodeSession,
  turn: parseCodeTurn,
  queued_turn: parseQueuedCodeTurn,
  // The queued list has no parser of its own; the rows do.
  queued_turns: (value) =>
    isRecord(value) && Array.isArray(value.queued)
      ? value.queued.map(parseQueuedCodeTurn).every(Boolean) || null
      : null,
  harness_doctor: parseHarnessDoctorReport,
  workspace_files: parseCodeWorkspaceFiles,
  workspace_diff: parseCodeWorkspaceDiff,
  approval: parseCodeApproval,
  commit: parseCodeCommit,
  push: parseCodePush,
  workspace_pr: parseCodeWorkspacePr,
  action: parseCodeAction,
  session_digest: parseCodeSessionDigest,
  update_notice: parseCodeUpdateNotice,
  event_frame: parseSequencedCodeEvent,
};

/**
 * Real server values the desktop still rejects, by fixture name. Empty
 * today; an entry here is a bug with an issue number, not a skip.
 */
const KNOWN_GAPS = new Set<string>([]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

describe("code frames against real server output", () => {
  it("carries every kind this file knows a parser for", () => {
    expect(CODE_FRAMES.length).toBeGreaterThan(40);
    const kinds = new Set(CODE_FRAMES.map(({ kind }) => kind));
    for (const kind of Object.keys(CODE_FRAME_PARSERS)) {
      expect(kinds.has(kind), kind).toBe(true);
    }
    for (const kind of kinds) {
      expect(CODE_FRAME_PARSERS[kind], `no parser for ${kind}`).toBeDefined();
    }
  });

  it("accepts every value the server serializes", () => {
    // Every other test here builds its own input, which encodes what the
    // author believed the wire looked like. These values come from the
    // server, so a field renamed there fails here rather than in the app.
    //
    // The fixtures that fail today are listed, not skipped: each one is a
    // real server value the desktop drops, and the entry has to go when its
    // parser catches up. The set is compared exactly, so a fix shows up as a
    // stale entry and a new gap shows up as a new name.
    const rejected = new Set<string>();
    for (const { name, kind, value } of CODE_FRAMES) {
      if (CODE_FRAME_PARSERS[kind]?.(value) === null) rejected.add(name);
    }
    expect(rejected).toEqual(KNOWN_GAPS);
  });

  it("keeps the frame flags the reducer reads", () => {
    // Before the fixtures, the socket parser dropped every frame that carried
    // `transient`, `replacement`, or `truncated`, so a live delta and a
    // capped replay never reached the reducer from this path.
    const flagged = CODE_FRAMES.filter(
      ({ kind, value }) =>
        kind === "event_frame" &&
        isRecord(value) &&
        ("transient" in value || "truncated" in value),
    );
    expect(flagged.length).toBeGreaterThan(1);
    for (const { name, value } of flagged) {
      const frame = parseSequencedCodeEvent(value);
      expect(frame, name).not.toBeNull();
      for (const flag of [
        "replayed",
        "transient",
        "replacement",
        "truncated",
      ]) {
        expect(
          (frame as Record<string, unknown>)[flag],
          `${name} ${flag}`,
        ).toBe((value as Record<string, unknown>)[flag]);
      }
    }
    expect(
      parseSequencedCodeEvent({
        seq: 1,
        event: { type: "turn_interrupted" },
        transient: "yes",
      }),
    ).toBeNull();
  });

  it("submits a turn as ran or queued by the server's two shapes", () => {
    const outcomes = new Set<string>();
    for (const { kind, value } of CODE_FRAMES) {
      if (kind !== "turn" && kind !== "queued_turn") continue;
      const submission = parseCodeTurnSubmission(value);
      expect(submission).not.toBeNull();
      outcomes.add(submission?.kind ?? "null");
    }
    expect(outcomes).toEqual(new Set(["ran", "queued"]));
  });
});
