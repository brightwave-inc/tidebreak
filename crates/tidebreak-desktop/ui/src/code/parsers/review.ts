import {
  isMember,
  isNonNegativeInteger,
  isPositiveInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeReviewFailure as WireCodeReviewFailure,
  CodeReviewFailureKind,
  CodeReviewFinding as WireCodeReviewFinding,
  CodeReviewList as WireCodeReviewList,
  CodeReviewProgress as WireCodeReviewProgress,
  CodeReviewResult as WireCodeReviewResult,
  CodeReviewSeverity,
  CodeReviewSnapshot as WireCodeReviewSnapshot,
  CodeReviewStatus,
} from "../../generated/wire";
import {
  nonEmptyLine,
  optionalLine,
  blockText,
  optionalBlock,
  rawText,
  wireId,
  optionalWireId,
  timestamp,
  optionalTimestamp,
  HARNESS_KINDS,
  PERMISSION_MODES,
} from "./shared";

const REVIEW_STATUSES = new Set<CodeReviewStatus>([
  "running",
  "completed",
  "failed",
  "cancelled",
  "timed_out",
]);
const REVIEW_FAILURE_KINDS = new Set<CodeReviewFailureKind>([
  "not_installed",
  "signed_out",
  "rate_limited",
  "timed_out",
  "failed",
]);
const REVIEW_SEVERITIES = new Set<CodeReviewSeverity>([
  "high",
  "medium",
  "low",
]);

function parseCodeReviewFinding(value: unknown): WireCodeReviewFinding | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeReviewFinding>(value, [
      "path",
      "start_line",
      "end_line",
      "severity",
      "title",
      "explanation",
    ]) ||
    !nonEmptyLine(value.path) ||
    !isPositiveInteger(value.start_line) ||
    !isPositiveInteger(value.end_line) ||
    value.end_line < value.start_line ||
    !isMember(value.severity, REVIEW_SEVERITIES) ||
    !nonEmptyLine(value.title) ||
    !blockText(value.explanation)
  ) {
    return null;
  }
  return {
    path: value.path,
    start_line: value.start_line,
    end_line: value.end_line,
    severity: value.severity,
    title: value.title,
    explanation: value.explanation,
  };
}

function parseCodeReviewFindings(
  value: unknown,
): WireCodeReviewFinding[] | null {
  if (!Array.isArray(value)) return null;
  const findings = value.map(parseCodeReviewFinding);
  return findings.every((finding) => finding !== null)
    ? (findings as WireCodeReviewFinding[])
    : null;
}

function parseCodeReviewResult(value: unknown): WireCodeReviewResult | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeReviewResult>(value, [
      "summary",
      "findings",
      "unplaced",
      "rejected",
      "raw_text",
      "diff",
      "omitted_diffs",
    ]) ||
    !optionalBlock(value.summary) ||
    !isNonNegativeInteger(value.rejected) ||
    !optionalBlock(value.raw_text) ||
    !rawText(value.diff) ||
    (value.omitted_diffs !== undefined &&
      !isNonNegativeInteger(value.omitted_diffs))
  ) {
    return null;
  }
  const findings = parseCodeReviewFindings(value.findings);
  const unplaced = parseCodeReviewFindings(value.unplaced);
  if (!findings || !unplaced) return null;
  return {
    ...(value.summary !== undefined ? { summary: value.summary } : {}),
    findings,
    unplaced,
    rejected: value.rejected,
    ...(value.raw_text !== undefined ? { raw_text: value.raw_text } : {}),
    diff: value.diff,
    ...(value.omitted_diffs !== undefined
      ? { omitted_diffs: value.omitted_diffs }
      : {}),
  };
}

function parseCodeReviewProgress(
  value: unknown,
): WireCodeReviewProgress | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeReviewProgress>(value, [
      "tool_calls",
      "files_read",
      "refused",
      "activity",
    ]) ||
    !isNonNegativeInteger(value.tool_calls) ||
    !isNonNegativeInteger(value.files_read) ||
    !isNonNegativeInteger(value.refused) ||
    !optionalLine(value.activity)
  ) {
    return null;
  }
  return {
    tool_calls: value.tool_calls,
    files_read: value.files_read,
    refused: value.refused,
    ...(value.activity !== undefined ? { activity: value.activity } : {}),
  };
}

function parseCodeReviewFailure(value: unknown): WireCodeReviewFailure | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeReviewFailure>(value, ["kind", "message"]) ||
    !isMember(value.kind, REVIEW_FAILURE_KINDS) ||
    !blockText(value.message)
  ) {
    return null;
  }
  return { kind: value.kind, message: value.message };
}

/**
 * One review of a workspace's changes by another engine. The findings are
 * engine output about code, so every field is held to the shape the server
 * promises before any of it reaches the diff.
 */
export function parseCodeReview(value: unknown): WireCodeReviewSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeReviewSnapshot>(value, [
      "id",
      "workspace_id",
      "session_id",
      "harness",
      "model",
      "turn_id",
      "permission_mode",
      "status",
      "progress",
      "started_at",
      "finished_at",
      "failure",
      "result",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.workspace_id) ||
    !wireId(value.session_id) ||
    !isMember(value.harness, HARNESS_KINDS) ||
    !optionalLine(value.model) ||
    !optionalWireId(value.turn_id) ||
    !isMember(value.permission_mode, PERMISSION_MODES) ||
    !isMember(value.status, REVIEW_STATUSES) ||
    !timestamp(value.started_at) ||
    !optionalTimestamp(value.finished_at)
  ) {
    return null;
  }
  const progress = parseCodeReviewProgress(value.progress);
  if (!progress) return null;
  const failure =
    value.failure === undefined
      ? undefined
      : parseCodeReviewFailure(value.failure);
  if (failure === null) return null;
  const result =
    value.result === undefined
      ? undefined
      : parseCodeReviewResult(value.result);
  if (result === null) return null;
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    session_id: value.session_id,
    harness: value.harness,
    ...(value.model !== undefined ? { model: value.model } : {}),
    ...(value.turn_id !== undefined ? { turn_id: value.turn_id } : {}),
    permission_mode: value.permission_mode,
    status: value.status,
    progress,
    started_at: value.started_at,
    ...(value.finished_at !== undefined
      ? { finished_at: value.finished_at }
      : {}),
    ...(failure ? { failure } : {}),
    ...(result ? { result } : {}),
  };
}

export function parseCodeReviewList(value: unknown): WireCodeReviewList | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeReviewList>(value, ["reviews"]) ||
    !Array.isArray(value.reviews)
  ) {
    return null;
  }
  const reviews = value.reviews.map(parseCodeReview);
  return reviews.every((review) => review !== null)
    ? { reviews: reviews as WireCodeReviewSnapshot[] }
    : null;
}
