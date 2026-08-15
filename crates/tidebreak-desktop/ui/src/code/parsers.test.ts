import { describe, expect, it } from "vitest";

import {
  liveCodeSession,
  parseCodeSession,
  parseCodeSessionList,
  parseCodeTurn,
  parseCodeTurnList,
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

describe("liveCodeSession", () => {
  it("prefers the newest non-ended session", () => {
    const ended = parseCodeSession({ ...SESSION, id: "old", lifecycle: "ended" });
    const live = parseCodeSession(SESSION);
    expect(ended && live).toBeTruthy();
    expect(liveCodeSession([ended!, live!])?.id).toBe("sess-1");
    expect(liveCodeSession([ended!])).toBeNull();
  });
});
