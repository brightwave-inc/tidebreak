import type {
  Attention,
  AttentionSource,
  AttentionState,
  CapLevel,
  CodeEvent,
  CodePermissionMode,
  CodeRepoSnapshot,
  CodeSessionLifecycle,
  CodeSessionSnapshot,
  CodeTurnSnapshot,
  CodeTurnStatus,
  CodeUsage,
  CodeWorkspaceSnapshot,
  CodeWorkspaceStatus,
  FenceReason,
  HarnessCaps,
  HarnessDoctorEntry,
  HarnessDoctorReport,
  HarnessKind,
  HarnessNoticeLevel,
  HarnessTier,
  SequencedCodeEventFrame,
  ToolDetail,
  ToolOutcome,
} from "../api/types";
import type {
  CodeEvent as WireCodeEvent,
  CodeRepoSnapshot as WireCodeRepoSnapshot,
  CodeSessionSnapshot as WireCodeSessionSnapshot,
  CodeTurnSnapshot as WireCodeTurnSnapshot,
  CodeWorkspaceSnapshot as WireCodeWorkspaceSnapshot,
  HarnessCaps as WireHarnessCaps,
  HarnessDoctorEntry as WireHarnessDoctorEntry,
  HarnessDoctorReport as WireHarnessDoctorReport,
  QuickAction as WireQuickAction,
  SequencedCodeEventFrame as WireSequencedCodeEventFrame,
  ToolDetail as WireToolDetail,
} from "../generated/wire";

/**
 * Runtime validators for every code-mode wire type the desktop consumes.
 *
 * Generated types describe the JSON; these functions decide whether a payload
 * is safe to keep. A field the server renamed or a notice that grew control
 * characters must fail here rather than render as if it were well-formed.
 */

const HARNESS_KINDS = new Set<HarnessKind>([
  "claude_code",
  "codex",
  "opencode",
  "grok",
]);
const HARNESS_TIERS = new Set<HarnessTier>([
  "reference",
  "secondary",
  "tertiary",
  "best_effort",
]);
const CAP_LEVELS = new Set<CapLevel>(["supported", "unsupported", "unknown"]);
const PERMISSION_MODES = new Set<CodePermissionMode>(["plan", "ask", "auto"]);
const SESSION_LIFECYCLES = new Set<CodeSessionLifecycle>([
  "created",
  "idle",
  "running",
  "fenced",
  "ended",
]);
const WORKSPACE_STATUSES = new Set<CodeWorkspaceStatus>([
  "creating",
  "setup_failed",
  "active",
  "archived",
]);
const TURN_STATUSES = new Set<CodeTurnStatus>([
  "running",
  "completed",
  "failed",
  "interrupted",
]);
const NOTICE_LEVELS = new Set<HarnessNoticeLevel>(["info", "warning", "error"]);
const TOOL_OUTCOMES = new Set<ToolOutcome>(["succeeded", "failed", "denied"]);
const ATTENTION_SOURCES = new Set<AttentionSource>([
  "structured",
  "heuristic",
  "lifecycle",
  "user",
]);

export function parseCodeRepo(value: unknown): CodeRepoSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeRepoSnapshot>(value, [
      "id",
      "root_path",
      "display_name",
      "default_base_ref",
      "branch_prefix",
      "setup_script",
      "archive_script",
      "quick_actions",
      "created_at",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.root_path) ||
    !nonEmpty(value.display_name) ||
    !nonEmpty(value.default_base_ref) ||
    !nonEmpty(value.branch_prefix) ||
    !nonEmpty(value.created_at) ||
    !optionalString(value.setup_script) ||
    !optionalString(value.archive_script) ||
    !Array.isArray(value.quick_actions)
  ) {
    return null;
  }
  const quick_actions = [];
  for (const action of value.quick_actions) {
    const parsed = parseQuickAction(action);
    if (!parsed) return null;
    quick_actions.push(parsed);
  }
  return {
    id: value.id,
    root_path: value.root_path,
    display_name: value.display_name,
    default_base_ref: value.default_base_ref,
    branch_prefix: value.branch_prefix,
    ...(value.setup_script !== undefined
      ? { setup_script: value.setup_script }
      : {}),
    ...(value.archive_script !== undefined
      ? { archive_script: value.archive_script }
      : {}),
    quick_actions,
    created_at: value.created_at,
  };
}

function parseQuickAction(value: unknown): WireQuickAction | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireQuickAction>(value, [
      "name",
      "command",
      "auto_run_on_create",
    ]) ||
    !nonEmpty(value.name) ||
    typeof value.command !== "string" ||
    typeof value.auto_run_on_create !== "boolean"
  ) {
    return null;
  }
  return {
    name: value.name,
    command: value.command,
    auto_run_on_create: value.auto_run_on_create,
  };
}

export function parseCodeWorkspace(
  value: unknown,
): CodeWorkspaceSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceSnapshot>(value, [
      "id",
      "repo_id",
      "title",
      "worktree_path",
      "branch_name",
      "base_ref",
      "status",
      "pr",
      "created_at",
      "archived_at",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.repo_id) ||
    !nonEmpty(value.title) ||
    !nonEmpty(value.worktree_path) ||
    !nonEmpty(value.branch_name) ||
    !nonEmpty(value.base_ref) ||
    !isMember(value.status, WORKSPACE_STATUSES) ||
    !nonEmpty(value.created_at) ||
    !optionalString(value.archived_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    repo_id: value.repo_id,
    title: value.title,
    worktree_path: value.worktree_path,
    branch_name: value.branch_name,
    base_ref: value.base_ref,
    status: value.status,
    created_at: value.created_at,
    ...(value.pr !== undefined ? { pr: value.pr as CodeWorkspaceSnapshot["pr"] } : {}),
    ...(value.archived_at !== undefined ? { archived_at: value.archived_at } : {}),
  };
}

export function parseCodeSession(value: unknown): CodeSessionSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeSessionSnapshot>(value, [
      "id",
      "workspace_id",
      "harness_kind",
      "harness_version",
      "harness_resume_ref",
      "permission_mode",
      "lifecycle",
      "fence_reason",
      "attention",
      "unrecognized_event_count",
      "created_at",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.workspace_id) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    !optionalString(value.harness_version) ||
    !optionalString(value.harness_resume_ref) ||
    !isMember(value.permission_mode, PERMISSION_MODES) ||
    !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
    !isFiniteNumber(value.unrecognized_event_count) ||
    !nonEmpty(value.created_at)
  ) {
    return null;
  }
  const attention = parseAttention(value.attention);
  if (!attention) return null;
  const fence_reason =
    value.fence_reason === undefined
      ? undefined
      : parseFenceReason(value.fence_reason);
  if (value.fence_reason !== undefined && !fence_reason) return null;
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    harness_kind: value.harness_kind,
    permission_mode: value.permission_mode,
    lifecycle: value.lifecycle,
    attention,
    unrecognized_event_count: value.unrecognized_event_count,
    created_at: value.created_at,
    ...(value.harness_version !== undefined
      ? { harness_version: value.harness_version }
      : {}),
    ...(value.harness_resume_ref !== undefined
      ? { harness_resume_ref: value.harness_resume_ref }
      : {}),
    ...(fence_reason ? { fence_reason } : {}),
  };
}

/** `GET /code/workspaces/{id}/sessions` — newest first. */
export function parseCodeSessionList(
  value: unknown,
): CodeSessionSnapshot[] | null {
  if (!Array.isArray(value)) return null;
  const sessions: CodeSessionSnapshot[] = [];
  for (const item of value) {
    const parsed = parseCodeSession(item);
    if (!parsed) return null;
    sessions.push(parsed);
  }
  return sessions;
}

/** The one session a workspace page should attach, if any. */
export function liveCodeSession(
  sessions: readonly CodeSessionSnapshot[],
): CodeSessionSnapshot | null {
  return sessions.find((session) => session.lifecycle !== "ended") ?? null;
}

export function parseCodeTurn(value: unknown): CodeTurnSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTurnSnapshot>(value, [
      "id",
      "session_id",
      "ordinal",
      "status",
      "user_input",
      "checkpoint_ref",
      "started_at",
      "ended_at",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.session_id) ||
    !isFiniteNumber(value.ordinal) ||
    !isMember(value.status, TURN_STATUSES) ||
    typeof value.user_input !== "string" ||
    !optionalString(value.checkpoint_ref) ||
    !nonEmpty(value.started_at) ||
    !optionalString(value.ended_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    session_id: value.session_id,
    ordinal: value.ordinal,
    status: value.status,
    user_input: value.user_input,
    started_at: value.started_at,
    ...(value.checkpoint_ref !== undefined
      ? { checkpoint_ref: value.checkpoint_ref }
      : {}),
    ...(value.ended_at !== undefined ? { ended_at: value.ended_at } : {}),
  };
}

export function parseHarnessDoctorReport(
  value: unknown,
): HarnessDoctorReport | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessDoctorReport>(value, ["harnesses"]) ||
    !Array.isArray(value.harnesses)
  ) {
    return null;
  }
  const harnesses: HarnessDoctorEntry[] = [];
  for (const entry of value.harnesses) {
    const parsed = parseHarnessDoctorEntry(entry);
    if (!parsed) return null;
    harnesses.push(parsed);
  }
  return { harnesses };
}

export function parseHarnessDoctorEntry(
  value: unknown,
): HarnessDoctorEntry | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessDoctorEntry>(value, [
      "kind",
      "found",
      "path",
      "version",
      "tier",
      "caps",
      "authenticated",
      "remediation",
      "stderr",
      "unrecognized_event_count",
    ]) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    typeof value.found !== "boolean" ||
    !optionalString(value.path) ||
    !optionalString(value.version) ||
    !isMember(value.tier, HARNESS_TIERS) ||
    (value.authenticated !== undefined &&
      typeof value.authenticated !== "boolean") ||
    typeof value.remediation !== "string" ||
    typeof value.stderr !== "string" ||
    !isFiniteNumber(value.unrecognized_event_count)
  ) {
    return null;
  }
  const caps = parseHarnessCaps(value.caps);
  if (!caps) return null;
  return {
    kind: value.kind,
    found: value.found,
    tier: value.tier,
    caps,
    remediation: value.remediation,
    stderr: value.stderr,
    unrecognized_event_count: value.unrecognized_event_count,
    ...(value.path !== undefined ? { path: value.path } : {}),
    ...(value.version !== undefined ? { version: value.version } : {}),
    ...(value.authenticated !== undefined
      ? { authenticated: value.authenticated }
      : {}),
  };
}

function parseHarnessCaps(value: unknown): HarnessCaps | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessCaps>(value, [
      "resume",
      "streaming_deltas",
      "structured_approvals",
      "mid_turn_steering",
      "plan_mode",
      "reasoning_levels",
      "native_file_change_events",
      "native_interrupt",
    ]) ||
    !isMember(value.resume, CAP_LEVELS) ||
    !isMember(value.streaming_deltas, CAP_LEVELS) ||
    !isMember(value.structured_approvals, CAP_LEVELS) ||
    !isMember(value.mid_turn_steering, CAP_LEVELS) ||
    !isMember(value.plan_mode, CAP_LEVELS) ||
    !isMember(value.reasoning_levels, CAP_LEVELS) ||
    !isMember(value.native_file_change_events, CAP_LEVELS) ||
    !isMember(value.native_interrupt, CAP_LEVELS)
  ) {
    return null;
  }
  return {
    resume: value.resume,
    streaming_deltas: value.streaming_deltas,
    structured_approvals: value.structured_approvals,
    mid_turn_steering: value.mid_turn_steering,
    plan_mode: value.plan_mode,
    reasoning_levels: value.reasoning_levels,
    native_file_change_events: value.native_file_change_events,
    native_interrupt: value.native_interrupt,
  };
}

export function parseSequencedCodeEvent(
  value: unknown,
): SequencedCodeEventFrame | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireSequencedCodeEventFrame>(value, ["seq", "event", "replayed"]) ||
    !isFiniteNumber(value.seq) ||
    (value.replayed !== undefined && typeof value.replayed !== "boolean")
  ) {
    return null;
  }
  const event = parseCodeEvent(value.event);
  if (!event) return null;
  return {
    seq: value.seq,
    event,
    ...(value.replayed !== undefined ? { replayed: value.replayed } : {}),
  };
}

export function parseCodeEvent(value: unknown): CodeEvent | null {
  if (
    !isRecord(value) ||
    typeof value.type !== "string" ||
    value.type.length === 0
  ) {
    return null;
  }
  switch (value.type) {
    case "session_started":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "session_started" }>>(value, [
          "type",
          "harness_kind",
          "harness_version",
          "resume_ref",
        ]) ||
        !isMember(value.harness_kind, HARNESS_KINDS) ||
        !nonEmpty(value.harness_version) ||
        !optionalString(value.resume_ref)
      ) {
        return null;
      }
      return {
        type: "session_started",
        harness_kind: value.harness_kind,
        harness_version: value.harness_version,
        ...(value.resume_ref !== undefined ? { resume_ref: value.resume_ref } : {}),
      };
    case "turn_started":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_started" }>>(value, [
          "type",
          "turn_id",
        ]) ||
        !nonEmpty(value.turn_id)
      ) {
        return null;
      }
      return { type: "turn_started", turn_id: value.turn_id };
    case "assistant_delta":
    case "assistant_message":
    case "reasoning_delta":
    case "user_steered":
      if (
        !onlyKeys(value, ["type", "text"]) ||
        typeof value.text !== "string"
      ) {
        return null;
      }
      return { type: value.type, text: value.text } as CodeEvent;
    case "tool_started": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "tool_started" }>>(value, [
          "type",
          "call_id",
          "name",
          "detail",
        ]) ||
        !nonEmpty(value.call_id) ||
        !nonEmpty(value.name)
      ) {
        return null;
      }
      const detail = parseToolDetail(value.detail);
      if (!detail) return null;
      return {
        type: "tool_started",
        call_id: value.call_id,
        name: value.name,
        detail,
      };
    }
    case "tool_completed":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "tool_completed" }>>(value, [
          "type",
          "call_id",
          "outcome",
          "preview",
        ]) ||
        !nonEmpty(value.call_id) ||
        !isMember(value.outcome, TOOL_OUTCOMES) ||
        typeof value.preview !== "string"
      ) {
        return null;
      }
      return {
        type: "tool_completed",
        call_id: value.call_id,
        outcome: value.outcome,
        preview: value.preview,
      };
    case "turn_completed": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_completed" }>>(value, [
          "type",
          "usage",
          "checkpoint",
        ])
      ) {
        return null;
      }
      const usage = parseUsage(value.usage);
      if (!usage) return null;
      return {
        type: "turn_completed",
        usage,
        ...(value.checkpoint !== undefined
          ? {
              checkpoint: value.checkpoint as Extract<
                WireCodeEvent,
                { type: "turn_completed" }
              >["checkpoint"],
            }
          : {}),
      };
    }
    case "turn_failed":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_failed" }>>(value, [
          "type",
          "error",
        ]) ||
        !isRecord(value.error) ||
        typeof value.error.message !== "string"
      ) {
        return null;
      }
      return { type: "turn_failed", error: { message: value.error.message } };
    case "turn_interrupted":
      return { type: "turn_interrupted" };
    case "harness_notice":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "harness_notice" }>>(value, [
          "type",
          "level",
          "message",
        ]) ||
        !isMember(value.level, NOTICE_LEVELS) ||
        typeof value.message !== "string"
      ) {
        return null;
      }
      return {
        type: "harness_notice",
        level: value.level,
        message: value.message,
      };
    case "attention_changed": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "attention_changed" }>>(value, [
          "type",
          "state",
          "source",
        ]) ||
        !isMember(value.source, ATTENTION_SOURCES)
      ) {
        return null;
      }
      const state = parseAttentionState(value.state);
      if (!state) return null;
      return { type: "attention_changed", state, source: value.source };
    }
    default:
      // Unknown kinds stay well-formed so a newer journal does not stall the
      // cursor. The reducer advances seq and paints nothing.
      return value as CodeEvent;
  }
}

function parseToolDetail(value: unknown): ToolDetail | null {
  if (!isRecord(value) || typeof value.kind !== "string") return null;
  switch (value.kind) {
    case "command":
      if (
        !onlyKeys<Extract<WireToolDetail, { kind: "command" }>>(value, [
          "kind",
          "cmd",
          "cwd",
        ]) ||
        typeof value.cmd !== "string" ||
        typeof value.cwd !== "string"
      ) {
        return null;
      }
      return { kind: "command", cmd: value.cmd, cwd: value.cwd };
    case "file_edit":
    case "file_read":
      if (
        !onlyKeys(value, ["kind", "path"]) ||
        typeof value.path !== "string"
      ) {
        return null;
      }
      return { kind: value.kind, path: value.path } as ToolDetail;
    case "search":
      if (
        !onlyKeys<Extract<WireToolDetail, { kind: "search" }>>(value, [
          "kind",
          "query",
        ]) ||
        typeof value.query !== "string"
      ) {
        return null;
      }
      return { kind: "search", query: value.query };
    case "other":
      if (
        !onlyKeys<Extract<WireToolDetail, { kind: "other" }>>(value, [
          "kind",
          "summary",
        ]) ||
        typeof value.summary !== "string"
      ) {
        return null;
      }
      return { kind: "other", summary: value.summary };
    default:
      return null;
  }
}

function parseUsage(value: unknown): CodeUsage | null {
  if (
    !isRecord(value) ||
    !isFiniteNumber(value.input_tokens) ||
    !isFiniteNumber(value.output_tokens) ||
    !isFiniteNumber(value.cache_read_input_tokens) ||
    !isFiniteNumber(value.cache_creation_input_tokens)
  ) {
    return null;
  }
  return {
    input_tokens: value.input_tokens,
    output_tokens: value.output_tokens,
    cache_read_input_tokens: value.cache_read_input_tokens,
    cache_creation_input_tokens: value.cache_creation_input_tokens,
  };
}

function parseAttention(value: unknown): Attention | null {
  if (!isRecord(value) || !isMember(value.source, ATTENTION_SOURCES)) {
    return null;
  }
  const state = parseAttentionState(value.state);
  if (!state) return null;
  return { state, source: value.source };
}

function parseAttentionState(value: unknown): AttentionState | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  switch (value.type) {
    case "working":
    case "done_unreviewed":
      return { type: value.type };
    case "needs_you":
      if (
        typeof value.prompt !== "string" ||
        !isMember(value.source, ATTENTION_SOURCES)
      ) {
        return null;
      }
      return { type: "needs_you", prompt: value.prompt, source: value.source };
    case "stalled":
      if (!isFiniteNumber(value.idle_secs)) return null;
      return { type: "stalled", idle_secs: value.idle_secs };
    case "fenced": {
      const reason = parseFenceReason(value.reason);
      if (!reason) return null;
      return { type: "fenced", reason };
    }
    case "manual":
      if (typeof value.note !== "string") return null;
      return { type: "manual", note: value.note };
    default:
      return null;
  }
}

export function parseFenceReason(value: unknown): FenceReason | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  if (value.type === "orphan_alive") return { type: "orphan_alive" };
  if (value.type === "probe_ambiguous" && typeof value.detail === "string") {
    return { type: "probe_ambiguous", detail: value.detail };
  }
  return null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function onlyKeys<Wire>(
  value: Record<string, unknown>,
  allowed: readonly (keyof Wire & string)[],
): boolean {
  const set = new Set<string>(allowed);
  return Object.keys(value).every((key) => set.has(key));
}

function nonEmpty(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function optionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

function isMember<T extends string>(
  value: unknown,
  allowed: ReadonlySet<T>,
): value is T {
  return typeof value === "string" && allowed.has(value as T);
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}
