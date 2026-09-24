import {
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeEvent,
  CredentialRefusalReason,
  HarnessNoticeLevel,
  SequencedCodeEventFrame,
  ToolDetail,
  ToolOutcome,
} from "../../api/types";
import { parseToolActionPreview } from "../../api/parsers";
import type {
  Event as WireCodeEvent,
  CheckpointRestoreStatus,
  ReviewOutcome,
  SequencedEventFrame as WireSequencedCodeEventFrame,
  ToolDetail as WireToolDetail,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  blockText,
  rawText,
  wireId,
  optionalWireId,
  HARNESS_KINDS,
  FILE_CHANGE_KINDS,
} from "./shared";
import { parseCheckpointRestoreTarget, parseDiffstat } from "./files";
import { parseSessionTreeChildren, parseSessionTreeWait } from "./sessions";
import { parseTurnActor, parseUsage } from "./turns";

const RESTORE_STATUSES = new Set<CheckpointRestoreStatus>([
  "started",
  "completed",
  "failed",
  "partial",
]);
const REVIEW_OUTCOMES = new Set<ReviewOutcome>([
  "completed",
  "failed",
  "cancelled",
  "timed_out",
]);

const NOTICE_LEVELS = new Set<HarnessNoticeLevel>(["info", "warning", "error"]);
const CREDENTIAL_REFUSAL_REASONS = new Set<CredentialRefusalReason>([
  "connection_ended",
  "not_connected",
  "forge_refused",
]);

const TOOL_OUTCOMES = new Set<ToolOutcome>(["succeeded", "failed", "denied"]);

const APPROVAL_CLASSES = new Set(["read_only", "workspace", "sensitive"]);
const TOOL_APPROVAL_KINDS = new Set([
  "search_may_share_query_and_excerpts",
  "web_search_may_share_query",
  "web_extract_may_fetch_url",
  "exec_may_run_networked_command",
  "external_mcp_may_call_server",
  "workspace_may_modify_files",
  "delegate_may_run_background_agent",
  "code_session_may_run_repository_agent",
  "computer_may_control_app",
  "unsupported",
]);

function parseInternalApprovalRequest(
  value: unknown,
): import("../../generated/wire").InternalApprovalRequest | null {
  if (!isRecord(value)) return null;
  if (value.kind === "questions" || value.kind === "plan") {
    if (!onlyKeys(value, ["kind", "turn_id"]) || !wireId(value.turn_id)) {
      return null;
    }
    return { kind: value.kind, turn_id: value.turn_id };
  }
  if (value.kind !== "tool_use") return null;
  if (
    !onlyKeys(value, [
      "kind",
      "auto_judging",
      "tool_name",
      "class",
      "approval",
      "grant_scopes",
      "preview",
      "preview_truncated",
    ]) ||
    typeof value.tool_name !== "string" ||
    !APPROVAL_CLASSES.has(value.class as string) ||
    !TOOL_APPROVAL_KINDS.has(value.approval as string) ||
    (value.auto_judging !== undefined &&
      typeof value.auto_judging !== "boolean") ||
    (value.preview_truncated !== undefined &&
      typeof value.preview_truncated !== "boolean") ||
    (value.grant_scopes !== undefined && !Array.isArray(value.grant_scopes))
  ) {
    return null;
  }
  const preview =
    value.preview === undefined
      ? undefined
      : parseToolActionPreview(value.preview);
  if (value.preview !== undefined && !preview) return null;
  return {
    kind: "tool_use",
    ...(value.auto_judging ? { auto_judging: true } : {}),
    tool_name: value.tool_name,
    class: value.class as import("../../generated/wire").ApprovalClass,
    approval: value.approval as import("../../generated/wire").ToolApprovalKind,
    ...(value.grant_scopes ? { grant_scopes: value.grant_scopes } : {}),
    ...(preview ? { preview } : {}),
    ...(value.preview_truncated ? { preview_truncated: true } : {}),
  };
}

export function parseSequencedCodeEvent(
  value: unknown,
): SequencedCodeEventFrame | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireSequencedCodeEventFrame>(value, [
      "seq",
      "event",
      "replayed",
      "transient",
      "replacement",
      "truncated",
    ]) ||
    !isFiniteNumber(value.seq) ||
    !optionalBoolean(value.replayed) ||
    !optionalBoolean(value.transient) ||
    !optionalBoolean(value.replacement) ||
    !optionalBoolean(value.truncated)
  ) {
    return null;
  }
  const event = parseCodeEvent(value.event);
  if (!event) return null;
  // The reducer reads all four flags: `replayed` and `truncated` decide how
  // a capped replay lands, `transient` keeps a live delta from advancing the
  // cursor, and `replacement` swaps the buffered tail instead of appending.
  return {
    seq: value.seq,
    event,
    ...(value.replayed !== undefined ? { replayed: value.replayed } : {}),
    ...(value.transient !== undefined ? { transient: value.transient } : {}),
    ...(value.replacement !== undefined
      ? { replacement: value.replacement }
      : {}),
    ...(value.truncated !== undefined ? { truncated: value.truncated } : {}),
  };
}

function optionalBoolean(value: unknown): value is boolean | undefined {
  return value === undefined || typeof value === "boolean";
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
    case "background_activity": {
      // What the engine did on its own: one ordinary event, never another
      // wrapper.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "background_activity" }>>(
          value,
          ["type", "event"],
        )
      ) {
        return null;
      }
      const inner = parseCodeEvent(value.event);
      if (!inner || inner.type === "background_activity") return null;
      return { type: "background_activity", event: inner };
    }
    case "session_tree": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "session_tree" }>>(value, [
          "type",
          "children",
          "wait",
        ]) ||
        value.children === undefined ||
        value.wait === undefined
      ) {
        return null;
      }
      const children = parseSessionTreeChildren(value.children);
      const wait = parseSessionTreeWait(value.wait);
      if (children === undefined || wait === undefined) return null;
      return { type: "session_tree", children, wait };
    }
    case "session_started":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "session_started" }>>(value, [
          "type",
          "harness_kind",
          "harness_version",
          "resume_ref",
        ]) ||
        !isMember(value.harness_kind, HARNESS_KINDS) ||
        !nonEmptyLine(value.harness_version) ||
        !optionalLine(value.resume_ref)
      ) {
        return null;
      }
      return {
        type: "session_started",
        harness_kind: value.harness_kind,
        harness_version: value.harness_version,
        ...(value.resume_ref !== undefined
          ? { resume_ref: value.resume_ref }
          : {}),
      };
    case "model_reported":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "model_reported" }>>(value, [
          "type",
          "model",
        ]) ||
        !nonEmptyLine(value.model) ||
        value.model.length > 160
      ) {
        return null;
      }
      return { type: "model_reported", model: value.model };
    case "turn_started":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_started" }>>(value, [
          "type",
          "turn_id",
        ]) ||
        !wireId(value.turn_id)
      ) {
        return null;
      }
      return { type: "turn_started", turn_id: value.turn_id };
    case "turn_resumed":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_resumed" }>>(value, [
          "type",
          "turn_id",
        ]) ||
        !wireId(value.turn_id)
      ) {
        return null;
      }
      return { type: "turn_resumed", turn_id: value.turn_id };
    case "assistant_delta":
    case "reasoning_delta":
      if (!onlyKeys(value, ["type", "text"]) || !blockText(value.text)) {
        return null;
      }
      return { type: value.type, text: value.text } as CodeEvent;
    case "user_steered":
      // A steer is the user's own words, kept verbatim. The internal engine
      // names the transcript row the steer became; the chat surface uses
      // it, this one only carries it.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "user_steered" }>>(value, [
          "type",
          "text",
          "message_id",
        ]) ||
        !rawText(value.text) ||
        (value.message_id !== undefined && !wireId(value.message_id))
      ) {
        return null;
      }
      return {
        type: "user_steered",
        text: value.text,
        ...(value.message_id !== undefined
          ? { message_id: value.message_id }
          : {}),
      };
    case "assistant_message":
      // A subagent's message names its spanning `Task` call (ADR 0052).
      if (
        !onlyKeys(value, ["type", "text", "parent_call_id"]) ||
        !blockText(value.text) ||
        (value.parent_call_id !== undefined && !wireId(value.parent_call_id))
      ) {
        return null;
      }
      return {
        type: "assistant_message",
        text: value.text,
        ...(value.parent_call_id !== undefined
          ? { parent_call_id: value.parent_call_id }
          : {}),
      };
    case "tool_started": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "tool_started" }>>(value, [
          "type",
          "call_id",
          "name",
          "detail",
          "parent_call_id",
        ]) ||
        !wireId(value.call_id) ||
        !nonEmptyLine(value.name) ||
        (value.parent_call_id !== undefined && !wireId(value.parent_call_id))
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
        ...(value.parent_call_id !== undefined
          ? { parent_call_id: value.parent_call_id }
          : {}),
      };
    }
    case "tool_completed": {
      // The internal engine also journals the call's whole `output` and its
      // action and result previews. The chat surface renders those through
      // its own wire; this parser keeps the code view's fields and leaves
      // the rest on the row.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "tool_completed" }>>(value, [
          "type",
          "call_id",
          "outcome",
          "preview",
          "detail",
          "parent_call_id",
          "output",
          "action",
          "result",
        ]) ||
        !wireId(value.call_id) ||
        !isMember(value.outcome, TOOL_OUTCOMES) ||
        !rawText(value.preview) ||
        (value.parent_call_id !== undefined && !wireId(value.parent_call_id))
      ) {
        return null;
      }
      // The server omits `detail` when the engine's completion payload
      // carried no arguments. A present-but-malformed one is a wire
      // disagreement, so it rejects the event rather than dropping a field.
      let detail: ToolDetail | undefined;
      if (value.detail !== undefined && value.detail !== null) {
        const parsed = parseToolDetail(value.detail);
        if (!parsed) return null;
        detail = parsed;
      }
      return {
        type: "tool_completed",
        call_id: value.call_id,
        outcome: value.outcome,
        preview: value.preview,
        ...(detail ? { detail } : {}),
        ...(value.parent_call_id !== undefined
          ? { parent_call_id: value.parent_call_id }
          : {}),
      };
    }
    case "turn_completed": {
      // `stop_reason` is the internal engine's; the code view reads the turn
      // as completed either way.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_completed" }>>(value, [
          "type",
          "usage",
          "checkpoint",
          "stop_reason",
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
      // `detail` is the internal engine's machine-readable kind beside the
      // message; the code view shows the message.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_failed" }>>(value, [
          "type",
          "error",
          "detail",
        ]) ||
        !isRecord(value.error) ||
        !blockText(value.error.message)
      ) {
        return null;
      }
      return { type: "turn_failed", error: { message: value.error.message } };
    case "turn_refused": {
      // The internal engine's terminal for a model refusal: the turn is
      // over, the way a completion ends it.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_refused" }>>(value, [
          "type",
          "usage",
          "refusal",
        ]) ||
        !isRecord(value.refusal) ||
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_refused" }>["refusal"]>(
          value.refusal,
          ["details", "partial_output", "source"],
        ) ||
        !isRecord(value.refusal.details) ||
        typeof value.refusal.partial_output !== "boolean" ||
        !(
          value.refusal.source === undefined ||
          value.refusal.source === "report_blocked"
        ) ||
        !(
          value.refusal.details.category === null ||
          typeof value.refusal.details.category === "string"
        )
      ) {
        return null;
      }
      const usage = parseUsage(value.usage);
      if (!usage) return null;
      return {
        type: "turn_refused",
        usage,
        refusal: {
          details: { category: value.refusal.details.category },
          partial_output: value.refusal.partial_output,
          ...(value.refusal.source ? { source: value.refusal.source } : {}),
        },
      };
    }
    case "checkpoint_recorded": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "checkpoint_recorded" }>>(
          value,
          ["type", "turn_id", "diffstat"],
        ) ||
        !wireId(value.turn_id)
      ) {
        return null;
      }
      const diffstat = parseDiffstat(value.diffstat);
      if (!diffstat) return null;
      return {
        type: "checkpoint_recorded",
        turn_id: value.turn_id,
        diffstat,
      };
    }
    case "checkpoint_restored": {
      // A restore between turns: the id names the state it replaced, so the
      // transcript can offer to put that state back. It is journaled as
      // started before any file moves, then again with how it ended.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "checkpoint_restored" }>>(
          value,
          [
            "type",
            "restore_id",
            "target",
            "diffstat",
            "actor",
            "status",
            "error",
          ],
        ) ||
        !wireId(value.restore_id) ||
        (value.status !== undefined &&
          !isMember(value.status, RESTORE_STATUSES)) ||
        (value.error !== undefined && !blockText(value.error))
      ) {
        return null;
      }
      const target = parseCheckpointRestoreTarget(value.target);
      const diffstat = parseDiffstat(value.diffstat);
      if (!target || !diffstat) return null;
      const actor = parseTurnActor(value.actor);
      if (value.actor !== undefined && !actor) return null;
      return {
        type: "checkpoint_restored",
        restore_id: value.restore_id,
        target,
        diffstat,
        ...(actor ? { actor } : {}),
        status: value.status ?? "completed",
        ...(value.error !== undefined ? { error: value.error } : {}),
      };
    }
    case "review_finished": {
      // Another engine's read-only review ended. Journaled by the review
      // runner, never by an engine; the findings travel as comments.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "review_finished" }>>(value, [
          "type",
          "review_id",
          "harness",
          "model",
          "turn_id",
          "outcome",
          "findings",
        ]) ||
        !wireId(value.review_id) ||
        !isMember(value.harness, HARNESS_KINDS) ||
        !optionalLine(value.model) ||
        !optionalWireId(value.turn_id) ||
        !isMember(value.outcome, REVIEW_OUTCOMES) ||
        !isNonNegativeInteger(value.findings)
      ) {
        return null;
      }
      return {
        type: "review_finished",
        review_id: value.review_id,
        harness: value.harness,
        ...(value.model !== undefined ? { model: value.model } : {}),
        ...(value.turn_id !== undefined ? { turn_id: value.turn_id } : {}),
        outcome: value.outcome,
        findings: value.findings,
      };
    }
    case "turn_interrupted": {
      // The internal engine reports the usage up to the interruption.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "turn_interrupted" }>>(value, [
          "type",
          "usage",
        ])
      ) {
        return null;
      }
      if (value.usage === undefined) return { type: "turn_interrupted" };
      const usage = parseUsage(value.usage);
      if (!usage) return null;
      return { type: "turn_interrupted", usage };
    }
    case "harness_notice":
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "harness_notice" }>>(value, [
          "type",
          "level",
          "message",
        ]) ||
        !isMember(value.level, NOTICE_LEVELS) ||
        !blockText(value.message)
      ) {
        return null;
      }
      return {
        type: "harness_notice",
        level: value.level,
        message: value.message,
      };
    case "credential_refused":
      // The machine refused the session's own git a forge credential; the
      // reason class picks the remedy, the message is what the helper saw.
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "credential_refused" }>>(
          value,
          ["type", "reason", "message", "remediation"],
        ) ||
        !isMember(value.reason, CREDENTIAL_REFUSAL_REASONS) ||
        !blockText(value.message) ||
        !blockText(value.remediation)
      ) {
        return null;
      }
      return {
        type: "credential_refused",
        reason: value.reason,
        message: value.message,
        remediation: value.remediation,
      };
    case "approval_requested": {
      // Machine sessions journal the card's facts beside the id so a
      // channel adapter can render it; sandbox sessions carry none.
      if (
        !onlyKeys(value, ["type", "approval_id", "request"]) ||
        !wireId(value.approval_id)
      ) {
        return null;
      }
      if (value.request === undefined) {
        return { type: "approval_requested", approval_id: value.approval_id };
      }
      const request = parseInternalApprovalRequest(value.request);
      if (!request) return null;
      return {
        type: "approval_requested",
        approval_id: value.approval_id,
        request,
      };
    }
    case "approval_resolved": {
      if (
        !onlyKeys(value, ["type", "approval_id", "decision", "actor"]) ||
        !wireId(value.approval_id) ||
        !isRecord(value.decision) ||
        (value.decision.type !== "approve" &&
          value.decision.type !== "deny" &&
          value.decision.type !== "abandoned" &&
          value.decision.type !== "approved_with_grant" &&
          value.decision.type !== "answered" &&
          value.decision.type !== "plan_decided")
      ) {
        return null;
      }
      const actor = parseTurnActor(value.actor);
      if (value.actor !== undefined && !actor) return null;
      return {
        type: "approval_resolved",
        approval_id: value.approval_id,
        decision: value.decision as Extract<
          CodeEvent,
          { type: "approval_resolved" }
        >["decision"],
        ...(actor ? { actor } : {}),
      };
    }
    case "file_changed": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "file_changed" }>>(value, [
          "type",
          "path",
          "kind",
          "diffstat",
        ]) ||
        !lineText(value.path) ||
        !isMember(value.kind, FILE_CHANGE_KINDS)
      ) {
        return null;
      }
      const diffstat = parseDiffstat(value.diffstat);
      if (!diffstat) return null;
      return {
        type: "file_changed",
        path: value.path,
        kind: value.kind,
        diffstat,
      };
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
        !blockText(value.cmd) ||
        !lineText(value.cwd)
      ) {
        return null;
      }
      return { kind: "command", cmd: value.cmd, cwd: value.cwd };
    case "file_edit":
    case "file_read":
      if (!onlyKeys(value, ["kind", "path"]) || !lineText(value.path)) {
        return null;
      }
      return { kind: value.kind, path: value.path } as ToolDetail;
    case "search":
      if (
        !onlyKeys<Extract<WireToolDetail, { kind: "search" }>>(value, [
          "kind",
          "query",
        ]) ||
        !blockText(value.query)
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
        !blockText(value.summary)
      ) {
        return null;
      }
      return { kind: "other", summary: value.summary };
    default:
      return null;
  }
}
