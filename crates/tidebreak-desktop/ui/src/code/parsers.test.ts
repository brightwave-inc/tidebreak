import { describe, expect, it } from "vitest";

import {
  liveCodeSession,
  parseCodeSession,
  parseCodeSessionList,
  parseCodeTerminal,
  parseCodeTerminalList,
  parseCodeTerminalRead,
  parseCodeTurn,
  parseCodeTurnList,
  parseCodeWorkspaceDiff,
  parseCodeWorkspaceFiles,
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

describe("liveCodeSession", () => {
  it("prefers the newest non-ended session", () => {
    const ended = parseCodeSession({ ...SESSION, id: "old", lifecycle: "ended" });
    const live = parseCodeSession(SESSION);
    expect(ended && live).toBeTruthy();
    expect(liveCodeSession([ended!, live!])?.id).toBe("sess-1");
    expect(liveCodeSession([ended!])).toBeNull();
  });
});
