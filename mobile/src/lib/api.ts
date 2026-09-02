import type {
  CapLevel,
  CodeApprovalKind,
  CodeApprovalSnapshot,
  CodeSessionLifecycle,
  CodeTurnSnapshot,
  CodeWorkspaceStatus,
  HarnessAuthMode,
  HarnessCaps,
  HarnessKind,
  HarnessModel,
  HarnessModelSource,
  PermissionMode,
  QueuedCodeTurn,
  ReasoningEffort,
} from "../generated/wire";
import type { MachineClient } from "./machine";

export type CodeTurnSubmission =
  | { kind: "ran"; turn: CodeTurnSnapshot }
  | { kind: "queued"; queued: QueuedCodeTurn };

type MachineJsonClient = Pick<MachineClient, "getJson" | "requestJson">;

export type ActiveCodeWorkspace = {
  id: string;
  repo_id: string;
  title: string;
  branch_name: string;
  base_ref: string;
  status: CodeWorkspaceStatus;
  created_at: string;
};

export type CodeHarnessOption = {
  kind: HarnessKind;
  found: boolean;
  installable: boolean;
  version?: string;
  authenticated?: boolean;
  auth_mode: HarnessAuthMode;
  remediation: string;
  caps: HarnessCaps;
};

export type CodeHarnessModels = {
  kind: HarnessKind;
  models: HarnessModel[];
  reasoning_efforts: ReasoningEffort[];
  source: HarnessModelSource;
};

export type CodePermissionPolicy = {
  permission_mode_ceiling?: PermissionMode;
};

export type CreateCodeSessionInput = {
  harness: HarnessKind;
  permission_mode: PermissionMode;
  model?: string;
  reasoning_effort?: ReasoningEffort;
  fast_mode?: boolean;
};

export type CreatedCodeSession = {
  id: string;
  workspace_id: string;
  harness_kind: HarnessKind;
  permission_mode: PermissionMode;
  model?: string;
  reasoning_effort?: ReasoningEffort;
  fast_mode: boolean;
  lifecycle: CodeSessionLifecycle;
  created_at: string;
};

export type CodeSessionLaunchResult = {
  session: CreatedCodeSession;
  submitted: CodeTurnSubmission | null;
  undeliveredDraft: string | null;
  sendError: string | null;
};

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

function optionalBoolean(value: unknown): value is boolean | undefined {
  return value === undefined || typeof value === "boolean";
}

function finiteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function codeWorkspaceStatus(value: unknown): value is CodeWorkspaceStatus {
  return [
    "creating",
    "setup_failed",
    "active",
    "archiving",
    "archived",
    "released",
  ].includes(String(value));
}

function harnessKind(value: unknown): value is HarnessKind {
  return ["claude_code", "codex", "opencode", "grok", "internal"].includes(
    String(value),
  );
}

function harnessAuthMode(value: unknown): value is HarnessAuthMode {
  return [
    "local_sign_in",
    "gateway_managed",
    "gateway_relay",
    "hosted_unavailable",
  ].includes(String(value));
}

function capLevel(value: unknown): value is CapLevel {
  return ["supported", "unsupported", "unknown"].includes(String(value));
}

function permissionMode(value: unknown): value is PermissionMode {
  return ["plan", "ask", "auto", "allow"].includes(String(value));
}

function reasoningEffort(value: unknown): value is ReasoningEffort {
  return ["none", "low", "medium", "high", "xhigh", "max", "ultra"].includes(
    String(value),
  );
}

function codeSessionLifecycle(value: unknown): value is CodeSessionLifecycle {
  return ["created", "idle", "running", "fenced", "ended"].includes(
    String(value),
  );
}

function parseHarnessCaps(value: unknown): HarnessCaps | null {
  const caps = record(value);
  if (
    !caps ||
    !capLevel(caps.resume) ||
    !capLevel(caps.streaming_deltas) ||
    !capLevel(caps.structured_approvals) ||
    !capLevel(caps.mid_turn_steering) ||
    !capLevel(caps.plan_mode) ||
    !capLevel(caps.auto_mode) ||
    !capLevel(caps.allow_mode) ||
    !capLevel(caps.reasoning_levels) ||
    !capLevel(caps.native_file_change_events) ||
    !capLevel(caps.native_interrupt) ||
    !capLevel(caps.image_input) ||
    !capLevel(caps.slash_commands) ||
    !capLevel(caps.durable_parks) ||
    !capLevel(caps.user_questions) ||
    !capLevel(caps.standing_grants) ||
    !capLevel(caps.mid_turn_resume) ||
    !capLevel(caps.transcript)
  ) {
    return null;
  }
  return {
    resume: caps.resume,
    streaming_deltas: caps.streaming_deltas,
    structured_approvals: caps.structured_approvals,
    mid_turn_steering: caps.mid_turn_steering,
    plan_mode: caps.plan_mode,
    auto_mode: caps.auto_mode,
    allow_mode: caps.allow_mode,
    reasoning_levels: caps.reasoning_levels,
    native_file_change_events: caps.native_file_change_events,
    native_interrupt: caps.native_interrupt,
    image_input: caps.image_input,
    slash_commands: caps.slash_commands,
    durable_parks: caps.durable_parks,
    user_questions: caps.user_questions,
    standing_grants: caps.standing_grants,
    mid_turn_resume: caps.mid_turn_resume,
    transcript: caps.transcript,
  };
}

export function parseCodeWorkspace(
  value: unknown,
): ActiveCodeWorkspace | null {
  const workspace = record(value);
  if (
    !workspace ||
    !nonEmpty(workspace.id) ||
    !nonEmpty(workspace.repo_id) ||
    typeof workspace.title !== "string" ||
    !nonEmpty(workspace.branch_name) ||
    !nonEmpty(workspace.base_ref) ||
    !codeWorkspaceStatus(workspace.status) ||
    !nonEmpty(workspace.created_at)
  ) {
    return null;
  }
  return {
    id: workspace.id,
    repo_id: workspace.repo_id,
    title: workspace.title,
    branch_name: workspace.branch_name,
    base_ref: workspace.base_ref,
    status: workspace.status,
    created_at: workspace.created_at,
  };
}

export function parseCodeHarness(value: unknown): CodeHarnessOption | null {
  const harness = record(value);
  const caps = parseHarnessCaps(harness?.caps);
  if (
    !harness ||
    !harnessKind(harness.kind) ||
    typeof harness.found !== "boolean" ||
    typeof harness.installable !== "boolean" ||
    !optionalString(harness.version) ||
    !optionalBoolean(harness.authenticated) ||
    !harnessAuthMode(harness.auth_mode) ||
    typeof harness.remediation !== "string" ||
    !caps
  ) {
    return null;
  }
  return {
    kind: harness.kind,
    found: harness.found,
    installable: harness.installable,
    ...(harness.version !== undefined ? { version: harness.version } : {}),
    ...(harness.authenticated !== undefined
      ? { authenticated: harness.authenticated }
      : {}),
    auth_mode: harness.auth_mode,
    remediation: harness.remediation,
    caps,
  };
}

function parseHarnessModel(value: unknown): HarnessModel | null {
  const model = record(value);
  if (
    !model ||
    !nonEmpty(model.id) ||
    !nonEmpty(model.label) ||
    typeof model.default !== "boolean" ||
    !Array.isArray(model.reasoning_efforts) ||
    !model.reasoning_efforts.every(reasoningEffort) ||
    typeof model.fast_mode !== "boolean"
  ) {
    return null;
  }
  return {
    id: model.id,
    label: model.label,
    default: model.default,
    reasoning_efforts: model.reasoning_efforts,
    fast_mode: model.fast_mode,
  };
}

export function parseCodeHarnessModels(
  value: unknown,
): CodeHarnessModels | null {
  const listing = record(value);
  if (
    !listing ||
    !harnessKind(listing.kind) ||
    !Array.isArray(listing.models) ||
    !Array.isArray(listing.reasoning_efforts) ||
    !listing.reasoning_efforts.every(reasoningEffort) ||
    !["harness", "model_gateway"].includes(String(listing.source))
  ) {
    return null;
  }
  const models = listing.models.map(parseHarnessModel);
  if (models.some((model) => model === null)) return null;
  return {
    kind: listing.kind,
    models: models as HarnessModel[],
    reasoning_efforts: listing.reasoning_efforts,
    source: listing.source as HarnessModelSource,
  };
}

export function parseCreatedCodeSession(
  value: unknown,
): CreatedCodeSession | null {
  const session = record(value);
  if (
    !session ||
    !nonEmpty(session.id) ||
    !nonEmpty(session.workspace_id) ||
    !harnessKind(session.harness_kind) ||
    !permissionMode(session.permission_mode) ||
    !optionalString(session.model) ||
    !(
      session.reasoning_effort === undefined ||
      reasoningEffort(session.reasoning_effort)
    ) ||
    typeof session.fast_mode !== "boolean" ||
    !codeSessionLifecycle(session.lifecycle) ||
    !nonEmpty(session.created_at)
  ) {
    return null;
  }
  return {
    id: session.id,
    workspace_id: session.workspace_id,
    harness_kind: session.harness_kind,
    permission_mode: session.permission_mode,
    ...(session.model !== undefined ? { model: session.model } : {}),
    ...(session.reasoning_effort !== undefined
      ? { reasoning_effort: session.reasoning_effort }
      : {}),
    fast_mode: session.fast_mode,
    lifecycle: session.lifecycle,
    created_at: session.created_at,
  };
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
    case "tool_use":
      return record(kind.preview) &&
        Array.isArray(kind.offered_grants)
        ? (value as CodeApprovalKind)
        : null;
    case "questions":
      return Array.isArray(kind.questions) &&
        kind.questions.every((question) => {
          const entry = record(question);
          return entry !== null && typeof entry.question === "string";
        })
        ? (value as CodeApprovalKind)
        : null;
    case "plan":
      return typeof kind.proposed_mode === "string"
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
    !["running", "waiting", "completed", "failed", "interrupted"].includes(
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

export async function listActiveCodeWorkspaces(
  client: MachineJsonClient,
): Promise<ActiveCodeWorkspace[]> {
  return parseList(
    await client.getJson("/code/workspaces"),
    parseCodeWorkspace,
    "Code workspaces",
  ).filter((workspace) => workspace.status === "active");
}

export async function listCodeHarnesses(
  client: MachineJsonClient,
): Promise<CodeHarnessOption[]> {
  const report = record(await client.getJson("/code/harnesses"));
  if (!report || !Array.isArray(report.harnesses)) {
    throw new Error("Code harnesses response contains invalid data.");
  }
  return parseList(report.harnesses, parseCodeHarness, "Code harnesses");
}

export async function listCodeHarnessModels(
  client: MachineJsonClient,
  kind: HarnessKind,
): Promise<CodeHarnessModels> {
  const listing = required(
    parseCodeHarnessModels(
      await client.getJson(
        `/code/harnesses/${encodeURIComponent(kind)}/models`,
      ),
    ),
    "Code harness models",
  );
  if (listing.kind !== kind) {
    throw new Error("Code harness models response named a different harness.");
  }
  return listing;
}

export async function getCodePermissionPolicy(
  client: MachineJsonClient,
): Promise<CodePermissionPolicy> {
  const policy = record(await client.getJson("/policy"));
  if (!policy) {
    throw new Error("Machine policy response contains invalid data.");
  }
  if (
    policy.permission_mode_ceiling !== undefined &&
    !permissionMode(policy.permission_mode_ceiling)
  ) {
    throw new Error("Machine policy response contains invalid data.");
  }
  return policy.permission_mode_ceiling === undefined
    ? {}
    : { permission_mode_ceiling: policy.permission_mode_ceiling };
}

export async function createCodeSession(
  client: MachineJsonClient,
  workspaceId: string,
  input: CreateCodeSessionInput,
): Promise<CreatedCodeSession> {
  const body = {
    harness: input.harness,
    permission_mode: input.permission_mode,
    ...(input.model ? { model: input.model } : {}),
    ...(input.reasoning_effort
      ? { reasoning_effort: input.reasoning_effort }
      : {}),
    ...(input.fast_mode ? { fast_mode: true } : {}),
  };
  const session = required(
    parseCreatedCodeSession(
      await client.requestJson(
        `/code/workspaces/${encodeURIComponent(workspaceId)}/sessions`,
        { method: "POST", body, expectedStatus: 201 },
      ),
    ),
    "Code session",
  );
  if (
    session.workspace_id !== workspaceId ||
    session.harness_kind !== input.harness ||
    session.permission_mode !== input.permission_mode
  ) {
    throw new Error("Code session response did not match the launch request.");
  }
  return session;
}

export async function launchCodeSession(
  client: MachineJsonClient,
  workspaceId: string,
  input: CreateCodeSessionInput,
  firstMessage: string,
): Promise<CodeSessionLaunchResult> {
  const session = await createCodeSession(client, workspaceId, input);
  const submittedMessage = firstMessage.trim();
  if (!submittedMessage) {
    return {
      session,
      submitted: null,
      undeliveredDraft: null,
      sendError: null,
    };
  }
  try {
    return {
      session,
      submitted: await submitCodeTurn(client, session.id, submittedMessage),
      undeliveredDraft: null,
      sendError: null,
    };
  } catch (error) {
    return {
      session,
      submitted: null,
      undeliveredDraft: firstMessage,
      sendError:
        error instanceof Error
          ? error.message
          : "The first message could not be sent.",
    };
  }
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
