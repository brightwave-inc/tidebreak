import { describe, expect, it } from "vitest";

import {
  liveCodeSession,
  parseCodeAction,
  parseCodeApproval,
  parseCodeCloneDefaults,
  parseCodeCloneJob,
  parseCodeCommit,
  parseCodeEvent,
  parseCodePush,
  parseCodeSession,
  parseCodeSessionList,
  parseCodeSubscriptionUsage,
  parseCodeUpdateNotice,
  parseCodeTerminal,
  parseCodeTerminalList,
  parseCodeTerminalRead,
  parseCodeTurn,
  parseCodeTurnList,
  parseCodeTurnSubmission,
  parseCodeWorkspaceBlob,
  parseCodeWorkspaceDiff,
  parseCodeWorkspaceFiles,
  parseCodeWorkspaceSearch,
  parseCodeWorkspaceTree,
  parseCodeWorkspacePr,
  parseCodePrComments,
} from "./parsers";

const SESSION = {
  id: "sess-1",
  workspace_id: "ws-1",
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
    expect(parseCodeSessionList([SESSION, { ...SESSION, lifecycle: "paused" }])).toBeNull();
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
      session_id: "sess-1",
      message: "and run the tests",
      position: 1,
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
      matches: [
        { path: "src/lib.rs", line_number: 12, line: "fn crisp() {}" },
      ],
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
    expect(parseCodeWorkspaceFiles({ ...files, stat: { files: 1 } })).toBeNull();
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
    suggested_commit_message: "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)",
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
      suggested_commit_message: "Update workspace\n\n0 files changed, 0 insertions(+), 0 deletions(-)",
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
    expect(
      parseCodePrComments(one({ kind: "thread", body: "hi" })),
    ).toBeNull();
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
    expect(parseCodePush({ branch: "tidebreak/first", remote: "origin" })).toEqual({
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

describe("liveCodeSession", () => {
  it("prefers the newest non-ended session", () => {
    const ended = parseCodeSession({ ...SESSION, id: "old", lifecycle: "ended" });
    const live = parseCodeSession(SESSION);
    expect(ended && live).toBeTruthy();
    expect(liveCodeSession([ended!, live!])?.id).toBe("sess-1");
    expect(liveCodeSession([ended!])).toBeNull();
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
});
