import {
  isFiniteNumber,
  isMember,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeSessionActivity,
  CodeSessionDigest,
  CodeSubagentStatus,
  CodeSubagentSummary,
  CodeUpdateNotice,
  PullRequestDigest,
} from "../../api/types";
import type {
  SessionExternalOrigin as CodeSessionExternalOrigin,
  SessionDigest as WireCodeSessionDigest,
  UpdateNotice as WireCodeUpdateNotice,
  TurnRewriteState as CodeTurnRewriteState,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  optionalBlock,
  wireId,
  optionalWireId,
  nullableWireId,
  optionalTimestamp,
  HARNESS_KINDS,
  SESSION_LIFECYCLES,
  SESSION_KINDS,
  WATCH_STATES,
} from "./shared";
import {
  parseSessionTreeWait,
  parseAttention,
  parseFenceReason,
} from "./sessions";
import { parsePullRequestDigest } from "./workspaces";

const SESSION_ACTIVITIES = new Set<CodeSessionActivity>([
  "agent",
  "shell",
  "monitor",
  "subagents",
  "file",
  "search",
  "tool",
]);

const SUBAGENT_STATUSES = new Set<CodeSubagentStatus>([
  "running",
  "done",
  "failed",
]);

const TURN_REWRITE_STATES = new Set<CodeTurnRewriteState>([
  "rewriting",
  "rewritten",
  "failed",
]);

/** `undefined` stays undefined; a present list must be well-formed. */
function parseSubagents(value: unknown): CodeSubagentSummary[] | null {
  if (!Array.isArray(value)) return null;
  const subagents: CodeSubagentSummary[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !onlyKeys<CodeSubagentSummary>(item, ["call_id", "name", "status"]) ||
      !wireId(item.call_id) ||
      !lineText(item.name) ||
      !isMember(item.status, SUBAGENT_STATUSES)
    ) {
      return null;
    }
    subagents.push({
      call_id: item.call_id,
      name: item.name,
      status: item.status,
    });
  }
  return subagents;
}

function parseDigestOrigin(value: unknown): CodeSessionExternalOrigin | null {
  if (
    !isRecord(value) ||
    !onlyKeys<CodeSessionExternalOrigin>(value, [
      "channel_kind",
      "external_key",
    ]) ||
    !nonEmptyLine(value.channel_kind) ||
    !wireId(value.external_key)
  )
    return null;
  return { channel_kind: value.channel_kind, external_key: value.external_key };
}

export function parseCodeSessionDigest(
  value: unknown,
): CodeSessionDigest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeSessionDigest>(value, [
      "workspace",
      "can_open_chat",
      "session",
      "kind",
      "harness_kind",
      "lifecycle",
      "attention",
      "fence_reason",
      "title",
      "turn_count",
      "trigger_target_at",
      "external_origin",
      "activity",
      "activity_detail",
      "pr_state",
      "pr_count",
      "memory_proposal_count",
      "watch_state",
      "watch_detail",
      "watch_cycles",
      "subagents",
      "recap",
      "parent_session",
      "wait",
    ]) ||
    !nullableWireId(value.workspace) ||
    (value.can_open_chat !== undefined &&
      typeof value.can_open_chat !== "boolean") ||
    !wireId(value.session) ||
    !isMember(value.kind, SESSION_KINDS) ||
    (value.harness_kind !== undefined &&
      !isMember(value.harness_kind, HARNESS_KINDS)) ||
    !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
    !lineText(value.title) ||
    !isFiniteNumber(value.turn_count) ||
    !optionalTimestamp(value.trigger_target_at) ||
    (value.external_origin !== undefined &&
      !parseDigestOrigin(value.external_origin)) ||
    (value.activity !== undefined &&
      !isMember(value.activity, SESSION_ACTIVITIES)) ||
    !optionalLine(value.activity_detail) ||
    (value.pr_count !== undefined && !isFiniteNumber(value.pr_count)) ||
    (value.memory_proposal_count !== undefined &&
      !isFiniteNumber(value.memory_proposal_count)) ||
    (value.watch_state !== undefined &&
      !isMember(value.watch_state, WATCH_STATES)) ||
    !optionalBlock(value.watch_detail) ||
    (value.watch_cycles !== undefined && !isFiniteNumber(value.watch_cycles)) ||
    !optionalBlock(value.recap) ||
    (value.parent_session !== undefined && !wireId(value.parent_session))
  ) {
    return null;
  }
  const fence_reason =
    value.fence_reason === undefined
      ? undefined
      : parseFenceReason(value.fence_reason);
  if (value.fence_reason !== undefined && !fence_reason) return null;
  const attention = parseAttention(value.attention);
  if (!attention) return null;
  const pr_state =
    value.pr_state === undefined ? undefined : parsePrState(value.pr_state);
  if (value.pr_state !== undefined && !pr_state) return null;
  const subagents =
    value.subagents === undefined ? undefined : parseSubagents(value.subagents);
  const wait =
    value.wait === undefined ? undefined : parseSessionTreeWait(value.wait);
  if (value.wait !== undefined && wait === undefined && value.wait !== null)
    return null;
  if (value.subagents !== undefined && !subagents) return null;
  return {
    workspace: value.workspace,
    ...(value.can_open_chat !== undefined
      ? { can_open_chat: value.can_open_chat }
      : {}),
    session: value.session,
    kind: value.kind,
    ...(value.harness_kind !== undefined
      ? { harness_kind: value.harness_kind }
      : {}),
    lifecycle: value.lifecycle,
    attention,
    ...(fence_reason ? { fence_reason } : {}),
    title: value.title,
    ...(value.external_origin !== undefined
      ? { external_origin: parseDigestOrigin(value.external_origin)! }
      : {}),
    turn_count: value.turn_count,
    ...(value.trigger_target_at !== undefined
      ? { trigger_target_at: value.trigger_target_at }
      : {}),
    ...(value.activity !== undefined ? { activity: value.activity } : {}),
    ...(value.activity_detail !== undefined
      ? { activity_detail: value.activity_detail }
      : {}),
    ...(pr_state ? { pr_state } : {}),
    ...(value.pr_count !== undefined ? { pr_count: value.pr_count } : {}),
    ...(value.memory_proposal_count !== undefined
      ? { memory_proposal_count: value.memory_proposal_count }
      : {}),
    ...(value.watch_state !== undefined
      ? { watch_state: value.watch_state }
      : {}),
    ...(value.watch_detail !== undefined
      ? { watch_detail: value.watch_detail }
      : {}),
    ...(value.watch_cycles !== undefined
      ? { watch_cycles: value.watch_cycles }
      : {}),
    ...(subagents ? { subagents } : {}),
    ...(value.recap !== undefined ? { recap: value.recap } : {}),
    ...(value.parent_session !== undefined
      ? { parent_session: value.parent_session }
      : {}),
    ...(wait ? { wait } : {}),
  };
}

export function parseCodeUpdateNotice(value: unknown): CodeUpdateNotice | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  switch (value.type) {
    case "snapshot": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "snapshot" }>>(value, [
          "type",
          "sessions",
        ]) ||
        !Array.isArray(value.sessions)
      ) {
        return null;
      }
      const sessions: CodeSessionDigest[] = [];
      for (const item of value.sessions) {
        const parsed = parseCodeSessionDigest(item);
        if (!parsed) return null;
        sessions.push(parsed);
      }
      return { type: "snapshot", sessions };
    }
    case "digest": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "digest" }>>(value, [
          "type",
          "workspace",
          "session",
          "kind",
          "harness_kind",
          "lifecycle",
          "attention",
          "fence_reason",
          "title",
          "turn_count",
          "trigger_target_at",
          "external_origin",
          "activity",
          "activity_detail",
          "pr_state",
          "pr_count",
          "memory_proposal_count",
          "watch_state",
          "watch_detail",
          "watch_cycles",
          "subagents",
          "recap",
          "parent_session",
          "wait",
        ]) ||
        !nullableWireId(value.workspace) ||
        !wireId(value.session) ||
        !isMember(value.kind, SESSION_KINDS) ||
        (value.harness_kind !== undefined &&
          !isMember(value.harness_kind, HARNESS_KINDS)) ||
        !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
        !lineText(value.title) ||
        !isFiniteNumber(value.turn_count) ||
        !optionalTimestamp(value.trigger_target_at) ||
        (value.external_origin !== undefined &&
          !parseDigestOrigin(value.external_origin)) ||
        (value.activity !== undefined &&
          !isMember(value.activity, SESSION_ACTIVITIES)) ||
        !optionalLine(value.activity_detail) ||
        (value.pr_count !== undefined && !isFiniteNumber(value.pr_count)) ||
        (value.memory_proposal_count !== undefined &&
          !isFiniteNumber(value.memory_proposal_count)) ||
        (value.watch_state !== undefined &&
          !isMember(value.watch_state, WATCH_STATES)) ||
        !optionalBlock(value.watch_detail) ||
        (value.watch_cycles !== undefined &&
          !isFiniteNumber(value.watch_cycles)) ||
        !optionalBlock(value.recap) ||
        (value.parent_session !== undefined && !wireId(value.parent_session))
      ) {
        return null;
      }
      const fence_reason =
        value.fence_reason === undefined
          ? undefined
          : parseFenceReason(value.fence_reason);
      if (value.fence_reason !== undefined && !fence_reason) return null;
      const attention = parseAttention(value.attention);
      if (!attention) return null;
      const pr_state =
        value.pr_state === undefined ? undefined : parsePrState(value.pr_state);
      if (value.pr_state !== undefined && !pr_state) return null;
      const subagents =
        value.subagents === undefined
          ? undefined
          : parseSubagents(value.subagents);
      if (value.subagents !== undefined && !subagents) return null;
      const wait =
        value.wait === undefined ? undefined : parseSessionTreeWait(value.wait);
      if (value.wait !== undefined && wait === undefined && value.wait !== null)
        return null;
      return {
        type: "digest",
        workspace: value.workspace,
        session: value.session,
        kind: value.kind,
        ...(value.harness_kind !== undefined
          ? { harness_kind: value.harness_kind }
          : {}),
        lifecycle: value.lifecycle,
        attention,
        ...(fence_reason ? { fence_reason } : {}),
        title: value.title,
        ...(value.external_origin !== undefined
          ? { external_origin: parseDigestOrigin(value.external_origin)! }
          : {}),
        turn_count: value.turn_count,
        ...(value.trigger_target_at !== undefined
          ? { trigger_target_at: value.trigger_target_at }
          : {}),
        ...(value.activity !== undefined ? { activity: value.activity } : {}),
        ...(value.activity_detail !== undefined
          ? { activity_detail: value.activity_detail }
          : {}),
        ...(pr_state ? { pr_state } : {}),
        ...(value.pr_count !== undefined ? { pr_count: value.pr_count } : {}),
        ...(value.memory_proposal_count !== undefined
          ? { memory_proposal_count: value.memory_proposal_count }
          : {}),
        ...(value.watch_state !== undefined
          ? { watch_state: value.watch_state }
          : {}),
        ...(value.watch_detail !== undefined
          ? { watch_detail: value.watch_detail }
          : {}),
        ...(value.watch_cycles !== undefined
          ? { watch_cycles: value.watch_cycles }
          : {}),
        ...(subagents ? { subagents } : {}),
        ...(value.recap !== undefined ? { recap: value.recap } : {}),
        ...(value.parent_session !== undefined
          ? { parent_session: value.parent_session }
          : {}),
        ...(wait ? { wait } : {}),
      };
    }
    case "clone_progress": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "clone_progress" }>>(
          value,
          ["type", "job", "phase", "percent", "done", "error", "repo_id"],
        ) ||
        !wireId(value.job) ||
        !lineText(value.phase) ||
        typeof value.done !== "boolean" ||
        (value.percent !== undefined && !isFiniteNumber(value.percent)) ||
        !optionalBlock(value.error) ||
        !optionalWireId(value.repo_id)
      ) {
        return null;
      }
      return {
        type: "clone_progress",
        job: value.job,
        phase: value.phase,
        done: value.done,
        ...(value.percent !== undefined ? { percent: value.percent } : {}),
        ...(value.error !== undefined ? { error: value.error } : {}),
        ...(value.repo_id !== undefined ? { repo_id: value.repo_id } : {}),
      };
    }
    case "harness_install": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "harness_install" }>>(
          value,
          ["type", "kind", "version", "phase", "done", "error"],
        ) ||
        !isMember(value.kind, HARNESS_KINDS) ||
        !lineText(value.phase) ||
        typeof value.done !== "boolean" ||
        !optionalLine(value.version) ||
        !optionalBlock(value.error)
      ) {
        return null;
      }
      return {
        type: "harness_install",
        kind: value.kind,
        phase: value.phase,
        done: value.done,
        ...(value.version !== undefined ? { version: value.version } : {}),
        ...(value.error !== undefined ? { error: value.error } : {}),
      };
    }
    case "terminal_activity": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "terminal_activity" }>>(
          value,
          ["type", "workspace_id", "terminal_id"],
        ) ||
        !wireId(value.workspace_id) ||
        !wireId(value.terminal_id)
      ) {
        return null;
      }
      return {
        type: "terminal_activity",
        workspace_id: value.workspace_id,
        terminal_id: value.terminal_id,
      };
    }
    case "files_changed": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "files_changed" }>>(
          value,
          ["type", "workspace_id"],
        ) ||
        !wireId(value.workspace_id)
      ) {
        return null;
      }
      return { type: "files_changed", workspace_id: value.workspace_id };
    }
    case "turn_rewrite": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "turn_rewrite" }>>(
          value,
          ["type", "session", "turn_id", "state", "rewrite"],
        ) ||
        !wireId(value.session) ||
        !wireId(value.turn_id) ||
        !isMember(value.state, TURN_REWRITE_STATES) ||
        !optionalBlock(value.rewrite)
      ) {
        return null;
      }
      return {
        type: "turn_rewrite",
        session: value.session,
        turn_id: value.turn_id,
        state: value.state,
        ...(value.rewrite !== undefined ? { rewrite: value.rewrite } : {}),
      };
    }
    case "delivery": {
      // No payload: the delivery surface re-reads its query.
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "delivery" }>>(value, [
          "type",
        ])
      ) {
        return null;
      }
      return { type: "delivery" };
    }
    default:
      return null;
  }
}

function parsePrState(value: unknown): PullRequestDigest | null {
  return parsePullRequestDigest(value);
}
