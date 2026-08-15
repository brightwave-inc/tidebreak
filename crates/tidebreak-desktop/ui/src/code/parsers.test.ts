import { describe, expect, it } from "vitest";

import { liveCodeSession, parseCodeSession, parseCodeSessionList } from "./parsers";

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

describe("liveCodeSession", () => {
  it("prefers the newest non-ended session", () => {
    const ended = parseCodeSession({ ...SESSION, id: "old", lifecycle: "ended" });
    const live = parseCodeSession(SESSION);
    expect(ended && live).toBeTruthy();
    expect(liveCodeSession([ended!, live!])?.id).toBe("sess-1");
    expect(liveCodeSession([ended!])).toBeNull();
  });
});
