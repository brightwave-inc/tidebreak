import { describe, expect, it, vi } from "vitest";
import type { MachineClient } from "./machine";
import {
  createCodeSession,
  decideCodeApproval,
  getCodePermissionPolicy,
  interruptCodeSession,
  launchCodeSession,
  listActiveCodeWorkspaces,
  listCodeApprovals,
  listCodeHarnessModels,
  listCodeHarnesses,
  listCodeQueuedTurns,
  listCodeTurns,
  parseCodeApproval,
  parseCodeHarness,
  parseCodeHarnessModels,
  parseCodeWorkspace,
  parseCreatedCodeSession,
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

const caps = {
  resume: "supported",
  streaming_deltas: "supported",
  structured_approvals: "supported",
  mid_turn_steering: "supported",
  plan_mode: "supported",
  auto_mode: "supported",
  allow_mode: "supported",
  reasoning_levels: "supported",
  native_file_change_events: "supported",
  native_interrupt: "supported",
  image_input: "unsupported",
  slash_commands: "supported",
  durable_parks: "unsupported",
  user_questions: "unsupported",
  standing_grants: "unsupported",
  mid_turn_resume: "unsupported",
  transcript: "unsupported",
  memory_loopback: "unsupported",
};

const harness = {
  kind: "codex",
  found: true,
  installable: true,
  version: "1.0.0",
  tier: "reference",
  caps,
  commands: [],
  authenticated: true,
  auth_mode: "local_sign_in",
  remediation: "",
  stderr: "",
  unrecognized_event_count: 0,
  relaunch_composes_permission_mode: true,
};

const workspace = {
  id: "workspace-1",
  repo_id: "repo-1",
  title: "Mobile launch",
  worktree_path: "/workspace/mobile-launch",
  branch_name: "thet/mobile-launch",
  base_ref: "main",
  status: "active",
  created_at: "2026-08-27T00:00:00Z",
};

const createdSession = {
  id: "session-2",
  workspace_id: "workspace-1",
  kind: "interactive",
  harness_kind: "codex",
  permission_mode: "auto",
  model: "gpt-5.6-sol",
  fast_mode: false,
  lifecycle: "created",
  attention: { state: { type: "idle" }, source: "lifecycle" },
  unrecognized_event_count: 0,
  created_at: "2026-08-27T00:00:03Z",
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
  it("validates active workspaces instead of silently accepting partial rows", async () => {
    expect(parseCodeWorkspace(workspace)).toMatchObject({
      id: "workspace-1",
      status: "active",
    });
    expect(parseCodeWorkspace({ id: "workspace-1" })).toBeNull();

    const listed = fakeClient([
      workspace,
      { ...workspace, id: "workspace-2", status: "archived" },
    ]);
    await expect(listActiveCodeWorkspaces(listed.client)).resolves.toEqual([
      expect.objectContaining({ id: "workspace-1" }),
    ]);
    expect(listed.getJson).toHaveBeenCalledWith("/code/workspaces");

    const invalid = fakeClient([workspace, { ...workspace, branch_name: 42 }]);
    await expect(listActiveCodeWorkspaces(invalid.client)).rejects.toThrow(
      /invalid data/,
    );
  });

  it("validates harness capabilities and each listed model", async () => {
    expect(parseCodeHarness(harness)).toMatchObject({
      kind: "codex",
      caps: { structured_approvals: "supported" },
    });
    expect(
      parseCodeHarness({
        ...harness,
        caps: { ...caps, allow_mode: "maybe" },
      }),
    ).toBeNull();

    const doctor = fakeClient({ harnesses: [harness] });
    await expect(listCodeHarnesses(doctor.client)).resolves.toHaveLength(1);
    expect(doctor.getJson).toHaveBeenCalledWith("/code/harnesses");

    const listing = {
      kind: "codex",
      models: [
        {
          id: "gpt-5.6-sol",
          label: "GPT-5.6 Sol",
          default: true,
          reasoning_efforts: ["high", "ultra"],
          fast_mode: true,
        },
      ],
      reasoning_efforts: ["high", "ultra"],
      source: "model_gateway",
    };
    expect(parseCodeHarnessModels(listing)).toEqual(listing);
    expect(
      parseCodeHarnessModels({
        ...listing,
        models: [{ ...listing.models[0], fast_mode: "yes" }],
      }),
    ).toBeNull();

    const models = fakeClient(listing);
    await expect(
      listCodeHarnessModels(models.client, "codex"),
    ).resolves.toEqual(listing);
    expect(models.getJson).toHaveBeenCalledWith(
      "/code/harnesses/codex/models",
    );
  });

  it("reads the managed permission ceiling and rejects unknown modes", async () => {
    const capped = fakeClient({
      managed: true,
      source: "os",
      permission_mode_ceiling: "ask",
    });
    await expect(getCodePermissionPolicy(capped.client)).resolves.toEqual({
      permission_mode_ceiling: "ask",
    });

    const invalid = fakeClient({ permission_mode_ceiling: "danger" });
    await expect(getCodePermissionPolicy(invalid.client)).rejects.toThrow(
      /invalid data/,
    );
  });

  it("creates a session through the workspace route and checks the response identity", async () => {
    expect(parseCreatedCodeSession(createdSession)).toMatchObject({
      id: "session-2",
      workspace_id: "workspace-1",
    });
    expect(
      parseCreatedCodeSession({ ...createdSession, fast_mode: "false" }),
    ).toBeNull();

    const created = fakeClient({
      ...createdSession,
      workspace_id: "workspace/1",
    });
    await expect(
      createCodeSession(created.client, "workspace/1", {
        harness: "codex",
        permission_mode: "auto",
        model: "gpt-5.6-sol",
      }),
    ).resolves.toMatchObject({ id: "session-2" });
    expect(created.requestJson).toHaveBeenCalledWith(
      "/code/workspaces/workspace%2F1/sessions",
      {
        method: "POST",
        body: {
          harness: "codex",
          permission_mode: "auto",
          model: "gpt-5.6-sol",
        },
        expectedStatus: 201,
      },
    );

    const mismatched = fakeClient({
      ...createdSession,
      workspace_id: "workspace-elsewhere",
    });
    await expect(
      createCodeSession(mismatched.client, "workspace-1", {
        harness: "codex",
        permission_mode: "auto",
      }),
    ).rejects.toThrow(/did not match/);
  });

  it("keeps the first-message draft when the session starts but send fails", async () => {
    const getJson = vi.fn();
    const requestJson = vi
      .fn()
      .mockResolvedValueOnce(createdSession)
      .mockRejectedValueOnce(new Error("Connection ended"));
    const client = { getJson, requestJson };

    await expect(
      launchCodeSession(
        client,
        "workspace-1",
        {
          harness: "codex",
          permission_mode: "auto",
          model: "gpt-5.6-sol",
        },
        "  Fix the launch flow.  ",
      ),
    ).resolves.toEqual({
      session: expect.objectContaining({ id: "session-2" }),
      submitted: null,
      undeliveredDraft: "  Fix the launch flow.  ",
      sendError: "Connection ended",
    });
    expect(requestJson).toHaveBeenNthCalledWith(
      2,
      "/code/sessions/session-2/turns",
      {
        method: "POST",
        body: { message: "Fix the launch flow." },
        expectedStatus: 202,
      },
    );
  });

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
    // Structured kinds from an engine behind the adapter parse as cards too.
    expect(
      parseCodeApproval({
        ...approval,
        kind: {
          type: "tool_use",
          preview: { kind: "other", summary: "Run the export" },
          offered_grants: [],
        },
      }),
    ).not.toBeNull();
    expect(
      parseCodeApproval({
        ...approval,
        kind: {
          type: "questions",
          questions: [
            { id: "q1", question: "Which region?", options: [] },
          ],
        },
      }),
    ).not.toBeNull();
    expect(
      parseCodeApproval({
        ...approval,
        kind: { type: "plan", proposed_mode: "auto_edit" },
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
      "  Use the focused test.  ",
    );
    expect(denied.requestJson).toHaveBeenCalledWith(
      "/code/approvals/approval%2F1/decision",
      {
        method: "POST",
        body: { decision: "deny", feedback: "Use the focused test." },
        expectedStatus: 200,
      },
    );

    const missingFeedback = fakeClient(undefined);
    await expect(
      decideCodeApproval(missingFeedback.client, "approval-1", "deny", "   "),
    ).rejects.toThrow(/feedback is required/);
    expect(missingFeedback.requestJson).not.toHaveBeenCalled();

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
