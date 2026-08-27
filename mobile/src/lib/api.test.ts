import { describe, expect, it, vi } from "vitest";
import type { MachineClient } from "./machine";
import {
  decideCodeApproval,
  interruptCodeSession,
  listCodeApprovals,
  listCodeQueuedTurns,
  listCodeTurns,
  parseCodeApproval,
  parseCodeTurnSubmission,
  steerCodeSession,
  submitCodeTurn,
} from "./api";

const turn = {
  id: "turn-1",
  session_id: "session-1",
  ordinal: 1,
  status: "running",
  fast_mode: false,
  user_input: "Continue",
  attachments: [],
  started_at: "2026-08-27T00:00:00Z",
};

const queued = {
  id: "turn-2",
  session_id: "session-1",
  message: "After that",
  position: 0,
  created_at: "2026-08-27T00:00:01Z",
  updated_at: "2026-08-27T00:00:01Z",
};

const approval = {
  id: "approval-1",
  session_id: "session-1",
  turn_id: "turn-1",
  kind: { type: "command", cmd: "cargo test", cwd: "/workspace" },
  harness_raw_json: "{}",
  state: "pending",
  requested_at: "2026-08-27T00:00:02Z",
};

function fakeClient(response: unknown): {
  client: Pick<MachineClient, "getJson" | "requestJson">;
  getJson: ReturnType<typeof vi.fn>;
  requestJson: ReturnType<typeof vi.fn>;
} {
  const getJson = vi.fn(async () => response);
  const requestJson = vi.fn(async () => response);
  return { client: { getJson, requestJson }, getJson, requestJson };
}

describe("mobile supervision API contracts", () => {
  it("distinguishes an accepted turn from a queued follow-up", async () => {
    expect(parseCodeTurnSubmission(turn)).toEqual({ kind: "ran", turn });
    expect(parseCodeTurnSubmission(queued)).toEqual({ kind: "queued", queued });
    expect(parseCodeTurnSubmission({ session_id: "session-1" })).toBeNull();

    const running = fakeClient(turn);
    await expect(
      submitCodeTurn(running.client, "session/1", "Continue"),
    ).resolves.toMatchObject({ kind: "ran" });
    expect(running.requestJson).toHaveBeenCalledWith(
      "/code/sessions/session%2F1/turns",
      {
        method: "POST",
        body: { message: "Continue" },
        expectedStatus: 202,
      },
    );

    const parked = fakeClient(queued);
    await expect(
      submitCodeTurn(parked.client, "session-1", "After that"),
    ).resolves.toMatchObject({ kind: "queued" });
  });

  it("lists pending approvals and sends feedback only with a denial", async () => {
    expect(
      parseCodeApproval({
        ...approval,
        kind: { type: "command", cmd: "" },
      }),
    ).not.toBeNull();

    const listed = fakeClient([approval]);
    await expect(
      listCodeApprovals(listed.client, "session/1"),
    ).resolves.toHaveLength(1);
    expect(listed.getJson).toHaveBeenCalledWith(
      "/code/approvals?state=pending&session_id=session%2F1",
    );

    const denied = fakeClient({
      ...approval,
      state: "denied",
      feedback: "Use the focused test.",
      decided_at: "2026-08-27T00:00:03Z",
    });
    await decideCodeApproval(
      denied.client,
      "approval/1",
      "deny",
      "Use the focused test.",
    );
    expect(denied.requestJson).toHaveBeenCalledWith(
      "/code/approvals/approval%2F1/decision",
      {
        method: "POST",
        body: { decision: "deny", feedback: "Use the focused test." },
        expectedStatus: 200,
      },
    );

    const approved = fakeClient({
      ...approval,
      state: "approved",
      decided_at: "2026-08-27T00:00:03Z",
    });
    await decideCodeApproval(approved.client, "approval-1", "approve");
    expect(approved.requestJson).toHaveBeenCalledWith(
      "/code/approvals/approval-1/decision",
      {
        method: "POST",
        body: { decision: "approve" },
        expectedStatus: 200,
      },
    );
  });

  it("uses the current steer and interrupt routes", async () => {
    const client = fakeClient(undefined);
    await steerCodeSession(client.client, "session/1", "turn-1", "Try again");
    expect(client.requestJson).toHaveBeenNthCalledWith(
      1,
      "/code/sessions/session%2F1/steer",
      {
        method: "POST",
        body: { expected_turn_id: "turn-1", guidance: "Try again" },
        expectedStatus: 202,
      },
    );

    await interruptCodeSession(client.client, "session/1");
    expect(client.requestJson).toHaveBeenNthCalledWith(
      2,
      "/code/sessions/session%2F1/interrupt",
      { method: "POST", expectedStatus: 202 },
    );
  });

  it("validates turn and queue snapshots before reconciliation", async () => {
    const turns = fakeClient([turn]);
    await expect(listCodeTurns(turns.client, "session-1")).resolves.toEqual([
      turn,
    ]);

    const queue = fakeClient({ queued: [queued], paused: false });
    await expect(
      listCodeQueuedTurns(queue.client, "session-1"),
    ).resolves.toEqual({ queued: [queued], paused: false });

    const invalid = fakeClient({ queued: [{ id: "missing fields" }] });
    await expect(
      listCodeQueuedTurns(invalid.client, "session-1"),
    ).rejects.toThrow(/invalid data/);
  });
});
