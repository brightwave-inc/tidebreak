import type {
  CodeApprovalKind,
  CodeApprovalSnapshot,
  CodeTurnSnapshot,
  QueuedCodeTurn,
} from "../generated/wire";
import type { MachineClient } from "./machine";

export type CodeTurnSubmission =
  | { kind: "ran"; turn: CodeTurnSnapshot }
  | { kind: "queued"; queued: QueuedCodeTurn };

type MachineJsonClient = Pick<MachineClient, "getJson" | "requestJson">;

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
}

function nonEmpty(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function optionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

function finiteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function parseApprovalKind(value: unknown): CodeApprovalKind | null {
  const kind = record(value);
  if (!kind) return null;
  switch (kind.type) {
    case "command":
      return typeof kind.cmd === "string" &&
        (kind.cwd === undefined ||
          kind.cwd === null ||
          typeof kind.cwd === "string")
        ? (value as CodeApprovalKind)
        : null;
    case "file_write":
      return Array.isArray(kind.paths) &&
        kind.paths.every((path) => typeof path === "string")
        ? (value as CodeApprovalKind)
        : null;
    case "network":
    case "other":
      return typeof kind.summary === "string"
        ? (value as CodeApprovalKind)
        : null;
    default:
      return null;
  }
}

export function parseCodeApproval(value: unknown): CodeApprovalSnapshot | null {
  const approval = record(value);
  if (
    !approval ||
    !nonEmpty(approval.id) ||
    !nonEmpty(approval.session_id) ||
    !nonEmpty(approval.turn_id) ||
    !parseApprovalKind(approval.kind) ||
    typeof approval.harness_raw_json !== "string" ||
    !["pending", "approved", "denied", "abandoned"].includes(
      String(approval.state),
    ) ||
    !nonEmpty(approval.requested_at) ||
    !optionalString(approval.feedback) ||
    !optionalString(approval.decided_at)
  ) {
    return null;
  }
  return value as CodeApprovalSnapshot;
}

export function parseCodeTurn(value: unknown): CodeTurnSnapshot | null {
  const turn = record(value);
  if (
    !turn ||
    !nonEmpty(turn.id) ||
    !nonEmpty(turn.session_id) ||
    !finiteNumber(turn.ordinal) ||
    !["running", "completed", "failed", "interrupted"].includes(
      String(turn.status),
    ) ||
    !optionalString(turn.model) ||
    typeof turn.fast_mode !== "boolean" ||
    typeof turn.user_input !== "string" ||
    !Array.isArray(turn.attachments) ||
    !nonEmpty(turn.started_at) ||
    !optionalString(turn.ended_at)
  ) {
    return null;
  }
  return value as CodeTurnSnapshot;
}

export function parseQueuedCodeTurn(value: unknown): QueuedCodeTurn | null {
  const turn = record(value);
  if (
    !turn ||
    !nonEmpty(turn.id) ||
    !nonEmpty(turn.session_id) ||
    typeof turn.message !== "string" ||
    !finiteNumber(turn.position) ||
    !nonEmpty(turn.created_at) ||
    !nonEmpty(turn.updated_at)
  ) {
    return null;
  }
  return value as QueuedCodeTurn;
}

export function parseCodeTurnSubmission(
  value: unknown,
): CodeTurnSubmission | null {
  const turn = parseCodeTurn(value);
  if (turn) return { kind: "ran", turn };
  const queued = parseQueuedCodeTurn(value);
  return queued ? { kind: "queued", queued } : null;
}

function parseList<T>(
  value: unknown,
  parse: (item: unknown) => T | null,
  label: string,
): T[] {
  if (!Array.isArray(value)) {
    throw new Error(`${label} response is not an array.`);
  }
  return value.map((item) => {
    const parsed = parse(item);
    if (!parsed) throw new Error(`${label} response contains invalid data.`);
    return parsed;
  });
}

function required<T>(value: T | null, label: string): T {
  if (!value) throw new Error(`${label} response contains invalid data.`);
  return value;
}

export async function listCodeApprovals(
  client: MachineJsonClient,
  sessionId?: string,
): Promise<CodeApprovalSnapshot[]> {
  const params = new URLSearchParams({ state: "pending" });
  if (sessionId) params.set("session_id", sessionId);
  return parseList(
    await client.getJson(`/code/approvals?${params.toString()}`),
    parseCodeApproval,
    "Code approvals",
  );
}

export async function decideCodeApproval(
  client: MachineJsonClient,
  approvalId: string,
  decision: "approve" | "deny",
  feedback?: string,
): Promise<CodeApprovalSnapshot> {
  const denialFeedback = decision === "deny" ? feedback?.trim() : undefined;
  if (decision === "deny" && !denialFeedback) {
    throw new Error("Denial feedback is required.");
  }
  const body =
    decision === "deny"
      ? { decision, feedback: denialFeedback }
      : { decision };
  return required(
    parseCodeApproval(
      await client.requestJson(
        `/code/approvals/${encodeURIComponent(approvalId)}/decision`,
        { method: "POST", body, expectedStatus: 200 },
      ),
    ),
    "Code approval",
  );
}

export async function listCodeTurns(
  client: MachineJsonClient,
  sessionId: string,
): Promise<CodeTurnSnapshot[]> {
  return parseList(
    await client.getJson(
      `/code/sessions/${encodeURIComponent(sessionId)}/turns`,
    ),
    parseCodeTurn,
    "Code turns",
  );
}

export async function listCodeQueuedTurns(
  client: MachineJsonClient,
  sessionId: string,
): Promise<{ queued: QueuedCodeTurn[]; paused: boolean }> {
  const snapshot = record(
    await client.getJson(
      `/code/sessions/${encodeURIComponent(sessionId)}/queued`,
    ),
  );
  if (
    !snapshot ||
    !Array.isArray(snapshot.queued) ||
    typeof snapshot.paused !== "boolean"
  ) {
    throw new Error("Code queue response contains invalid data.");
  }
  return {
    queued: parseList(snapshot.queued, parseQueuedCodeTurn, "Code queue"),
    paused: snapshot.paused,
  };
}

export async function submitCodeTurn(
  client: MachineJsonClient,
  sessionId: string,
  message: string,
): Promise<CodeTurnSubmission> {
  return required(
    parseCodeTurnSubmission(
      await client.requestJson(
        `/code/sessions/${encodeURIComponent(sessionId)}/turns`,
        { method: "POST", body: { message }, expectedStatus: 202 },
      ),
    ),
    "Code turn",
  );
}

export async function steerCodeSession(
  client: MachineJsonClient,
  sessionId: string,
  expectedTurnId: string,
  guidance: string,
): Promise<void> {
  await client.requestJson(
    `/code/sessions/${encodeURIComponent(sessionId)}/steer`,
    {
      method: "POST",
      body: { expected_turn_id: expectedTurnId, guidance },
      expectedStatus: 202,
    },
  );
}

export async function interruptCodeSession(
  client: MachineJsonClient,
  sessionId: string,
): Promise<void> {
  await client.requestJson(
    `/code/sessions/${encodeURIComponent(sessionId)}/interrupt`,
    { method: "POST", expectedStatus: 202 },
  );
}
