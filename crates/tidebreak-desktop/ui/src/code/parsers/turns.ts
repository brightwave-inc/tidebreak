import {
  isFiniteNumber,
  isMember,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeApprovalSnapshot,
  CodeApprovalState,
  CodeTurnSnapshot,
  CodeTurnStatus,
  CodeUsage,
  QueuedCodeTurn,
} from "../../api/types";
import type {
  TurnSnapshot as WireCodeTurnSnapshot,
  QueuedTurn as WireQueuedCodeTurn,
  ImageRef as WireCodeTurnAttachment,
} from "../../generated/wire";
import {
  optionalLine,
  nullableLine,
  optionalBlock,
  rawText,
  wireId,
  timestamp,
  optionalTimestamp,
} from "./shared";
import { parseDiffstat } from "./files";
import { parsePullRequestChecks, TRIGGER_CONDITIONS } from "./workspaces";

const TURN_STATUSES = new Set<CodeTurnStatus>([
  "queued",
  "running",
  "waiting",
  "cancelling",
  "waiting_for_client",
  "waiting_for_agent_run",
  "cancelling_client",
  "resuming",
  "retry_wait",
  "completed",
  "failed",
  "interrupted",
]);

const APPROVAL_STATES = new Set<CodeApprovalState>([
  "pending",
  "approved",
  "denied",
  "abandoned",
]);

export function parseCodeTurn(value: unknown): CodeTurnSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTurnSnapshot>(value, [
      "id",
      "session_id",
      "ordinal",
      "status",
      "model",
      "fast_mode",
      "user_input",
      "attachments",
      "usage",
      "checkpoint_ref",
      "diffstat",
      "started_at",
      "ended_at",
      "rewrite",
      "actor",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.session_id) ||
    !isFiniteNumber(value.ordinal) ||
    !isMember(value.status, TURN_STATUSES) ||
    !optionalLine(value.model) ||
    // Serialized unconditionally, but tolerate its absence: a turn row
    // written before this snapshot existed reads as off, which is what
    // it was.
    (value.fast_mode !== undefined && typeof value.fast_mode !== "boolean") ||
    !optionalLine(value.checkpoint_ref) ||
    !rawText(value.user_input) ||
    !timestamp(value.started_at) ||
    !optionalTimestamp(value.ended_at) ||
    !optionalBlock(value.rewrite)
  ) {
    return null;
  }
  const actor = parseTurnActor(value.actor);
  if (value.actor !== undefined && !actor) return null;
  const attachments = parseCodeTurnAttachments(value.attachments);
  if (!attachments) return null;
  const usage = value.usage === undefined ? undefined : parseUsage(value.usage);
  if (value.usage !== undefined && !usage) return null;
  const diffstat =
    value.diffstat === undefined ? undefined : parseDiffstat(value.diffstat);
  if (value.diffstat !== undefined && !diffstat) return null;
  return {
    id: value.id,
    session_id: value.session_id,
    ordinal: value.ordinal,
    status: value.status,
    fast_mode: value.fast_mode === true,
    ...(value.model !== undefined ? { model: value.model } : {}),
    user_input: value.user_input,
    started_at: value.started_at,
    attachments,
    ...(usage ? { usage } : {}),
    ...(value.checkpoint_ref !== undefined
      ? { checkpoint_ref: value.checkpoint_ref }
      : {}),
    ...(diffstat ? { diffstat } : {}),
    ...(value.ended_at !== undefined ? { ended_at: value.ended_at } : {}),
    ...(value.rewrite !== undefined ? { rewrite: value.rewrite } : {}),
    ...(actor ? { actor } : {}),
  };
}

/**
 * Who submitted a turn or settled a decision (decision 0086).
 *
 * Every field is optional on the wire, so an object with all four null is
 * valid and simply names nobody. A display name is a channel's, not this
 * machine's, so it is bounded as a line like every other untrusted label.
 */
export function parseTurnActor(
  value: unknown,
): import("../../generated/wire").TurnActor | null {
  if (value === undefined) return null;
  if (
    !isRecord(value) ||
    !onlyKeys<import("../../generated/wire").TurnActor>(value, [
      "principal",
      "display",
      "channel_kind",
      "external_identity",
      "trigger",
    ]) ||
    !nullableLine(value.principal) ||
    !nullableLine(value.display) ||
    !nullableLine(value.channel_kind) ||
    !nullableLine(value.external_identity)
  ) {
    return null;
  }
  const trigger =
    value.trigger === undefined
      ? undefined
      : parseTriggerTurnContext(value.trigger);
  if (value.trigger !== undefined && !trigger) return null;
  return {
    principal: value.principal ?? null,
    display: value.display ?? null,
    channel_kind: value.channel_kind ?? null,
    external_identity: value.external_identity ?? null,
    ...(trigger ? { trigger } : {}),
  };
}

/**
 * The pull-request event a trigger or watch turn was fired on. The failing
 * checks reuse the digest's own check validator, so the event's list is
 * bounded exactly like the checks beside it.
 */
function parseTriggerTurnContext(
  value: unknown,
): import("../../generated/wire").TriggerTurnContext | null {
  if (
    !isRecord(value) ||
    !onlyKeys<import("../../generated/wire").TriggerTurnContext>(value, [
      "source",
      "condition",
      "pr_number",
      "pr_title",
      "pr_url",
      "head_sha",
      "failing_checks",
    ]) ||
    (value.source !== "trigger" && value.source !== "watch") ||
    !isMember(value.condition, TRIGGER_CONDITIONS) ||
    !isFiniteNumber(value.pr_number) ||
    !optionalLine(value.pr_title) ||
    !optionalLine(value.pr_url) ||
    !optionalLine(value.head_sha)
  ) {
    return null;
  }
  const failing = parsePullRequestChecks(value.failing_checks);
  if (!failing) return null;
  return {
    source: value.source,
    condition: value.condition,
    pr_number: value.pr_number,
    ...(value.pr_title !== undefined ? { pr_title: value.pr_title } : {}),
    ...(value.pr_url !== undefined ? { pr_url: value.pr_url } : {}),
    ...(value.head_sha !== undefined ? { head_sha: value.head_sha } : {}),
    ...(failing.length > 0 ? { failing_checks: failing } : {}),
  };
}

const IMAGE_MEDIA_TYPES = new Set<
  import("../../generated/wire").ImageMediaType
>(["png", "jpeg", "webp", "gif"]);

function parseCodeTurnAttachments(
  value: unknown,
): import("../../generated/wire").ImageRef[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;
  const attachments: import("../../generated/wire").ImageRef[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireCodeTurnAttachment>(item, [
        "blob_id",
        "media_type",
        "width",
        "height",
        "byte_len",
      ]) ||
      !wireId(item.blob_id) ||
      !isMember(item.media_type, IMAGE_MEDIA_TYPES) ||
      !isFiniteNumber(item.width) ||
      !isFiniteNumber(item.height) ||
      !isFiniteNumber(item.byte_len)
    ) {
      return null;
    }
    attachments.push({
      blob_id: item.blob_id,
      media_type: item.media_type,
      width: item.width,
      height: item.height,
      byte_len: item.byte_len,
    });
  }
  return attachments;
}

/**
 * What `POST /sessions/{id}/turns` did with the message.
 *
 * The route answers 202 for both outcomes: a turn that ran, or a follow-up
 * parked in the session's single queue slot while the current turn finishes.
 * The two payloads share no required key, so the shape discriminates.
 */
export type CodeTurnSubmission =
  | { kind: "ran"; turn: CodeTurnSnapshot }
  | { kind: "queued"; queued: QueuedCodeTurn };

export function parseQueuedCodeTurn(value: unknown): QueuedCodeTurn | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireQueuedCodeTurn>(value, [
      "id",
      "session_id",
      "message",
      "actor",
      "position",
      "created_at",
      "updated_at",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.session_id) ||
    !rawText(value.message) ||
    !isFiniteNumber(value.position) ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at)
  ) {
    return null;
  }
  const actor = parseTurnActor(value.actor);
  if (value.actor !== undefined && !actor) return null;
  return {
    id: value.id,
    session_id: value.session_id,
    message: value.message,
    position: value.position,
    created_at: value.created_at,
    updated_at: value.updated_at,
    ...(actor ? { actor } : {}),
  };
}

export function parseCodeTurnSubmission(
  value: unknown,
): CodeTurnSubmission | null {
  const turn = parseCodeTurn(value);
  if (turn) return { kind: "ran", turn };
  const queued = parseQueuedCodeTurn(value);
  return queued ? { kind: "queued", queued } : null;
}

/** `GET /sessions/{id}/turns` — oldest first. */
export function parseCodeTurnList(value: unknown): CodeTurnSnapshot[] | null {
  if (!Array.isArray(value)) return null;
  const turns: CodeTurnSnapshot[] = [];
  for (const item of value) {
    const parsed = parseCodeTurn(item);
    if (!parsed) return null;
    turns.push(parsed);
  }
  return turns;
}

export function parseUsage(value: unknown): CodeUsage | null {
  if (
    !isRecord(value) ||
    !isFiniteNumber(value.input_tokens) ||
    !isFiniteNumber(value.output_tokens) ||
    !isFiniteNumber(value.cache_read_input_tokens) ||
    !isFiniteNumber(value.cache_creation_input_tokens)
  ) {
    return null;
  }
  // `context_tokens` is serde-defaulted, so a turn journaled before the field
  // existed omits it. Rejecting the whole object over a missing occupancy
  // reading would throw away the spend counts beside it; absent reads as no
  // reading, which is what zero already means here.
  const contextTokens = value.context_tokens;
  if (contextTokens !== undefined && !isFiniteNumber(contextTokens)) {
    return null;
  }
  const firstCallContextTokens = value.first_call_context_tokens;
  if (
    firstCallContextTokens !== undefined &&
    !isFiniteNumber(firstCallContextTokens)
  ) {
    return null;
  }
  return {
    input_tokens: value.input_tokens,
    output_tokens: value.output_tokens,
    cache_read_input_tokens: value.cache_read_input_tokens,
    cache_creation_input_tokens: value.cache_creation_input_tokens,
    context_tokens: contextTokens ?? 0,
    ...(firstCallContextTokens === undefined
      ? {}
      : { first_call_context_tokens: firstCallContextTokens }),
  };
}

export function parseCodeApproval(value: unknown): CodeApprovalSnapshot | null {
  if (
    !isRecord(value) ||
    !wireId(value.id) ||
    !wireId(value.session_id) ||
    !wireId(value.turn_id) ||
    !isRecord(value.kind) ||
    typeof value.kind.type !== "string" ||
    !rawText(value.harness_raw_json) ||
    !isMember(value.state, APPROVAL_STATES) ||
    !timestamp(value.requested_at) ||
    !optionalBlock(value.feedback) ||
    !optionalTimestamp(value.decided_at)
  ) {
    return null;
  }
  const actor = parseTurnActor(value.actor);
  if (value.actor !== undefined && !actor) return null;
  return {
    id: value.id,
    session_id: value.session_id,
    turn_id: value.turn_id,
    kind: value.kind as CodeApprovalSnapshot["kind"],
    harness_raw_json: value.harness_raw_json,
    state: value.state,
    requested_at: value.requested_at,
    ...(value.feedback !== undefined ? { feedback: value.feedback } : {}),
    ...(value.decided_at !== undefined ? { decided_at: value.decided_at } : {}),
    ...(actor ? { actor } : {}),
  };
}
