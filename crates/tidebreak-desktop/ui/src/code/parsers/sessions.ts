import {
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  Attention,
  AttentionSource,
  AttentionState,
  CodeSessionSnapshot,
  FenceReason,
  ReasoningEffort,
  CodeForkTranscript,
} from "../../api/types";
import type {
  SessionSnapshot as WireCodeSessionSnapshot,
  SessionExternalOrigin as CodeSessionExternalOrigin,
  CodeForkTranscript as WireCodeForkTranscript,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  blockText,
  wireId,
  nullableWireId,
  timestamp,
  HARNESS_KINDS,
  PERMISSION_MODES,
  REASONING_EFFORTS,
  SESSION_LIFECYCLES,
  SESSION_KINDS,
} from "./shared";

const SESSION_VISIBILITIES = new Set<
  import("../../generated/wire").SessionVisibility
>(["private", "deployment"]);
const EXECUTION_LOCATIONS = new Set<"sandbox" | "machine">([
  "sandbox",
  "machine",
]);
const ACTS_AS = new Set<"person" | "bot">(["person", "bot"]);

const ATTENTION_SOURCES = new Set<AttentionSource>([
  "structured",
  "heuristic",
  "lifecycle",
  "user",
]);

export function parseCodeForkTranscript(
  value: unknown,
): CodeForkTranscript | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeForkTranscript>(value, [
      "path",
      "turns",
      "at_turn_ordinal",
      "truncated",
    ]) ||
    !lineText(value.path) ||
    typeof value.turns !== "number" ||
    (value.at_turn_ordinal !== undefined &&
      !isFiniteNumber(value.at_turn_ordinal)) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  return {
    path: value.path,
    turns: value.turns,
    ...(value.at_turn_ordinal !== undefined
      ? { at_turn_ordinal: value.at_turn_ordinal }
      : {}),
    truncated: value.truncated,
  };
}

const SESSION_TREE_CHILD_STATUSES = new Set([
  "running",
  "queued",
  "completed",
  "fenced",
  "failed",
  "interrupted",
]);

export function parseSessionTreeChildren(
  value: unknown,
): CodeSessionSnapshot["children"] | undefined {
  if (value === undefined) return undefined;
  if (!Array.isArray(value)) return undefined;
  const children: NonNullable<CodeSessionSnapshot["children"]> = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !onlyKeys(item, [
        "id",
        "title",
        "status",
        "attention",
        "fenced",
        "workspace_id",
        "execution_location",
      ]) ||
      !wireId(item.id) ||
      (item.title !== undefined && !nonEmptyLine(item.title)) ||
      typeof item.status !== "string" ||
      !SESSION_TREE_CHILD_STATUSES.has(item.status) ||
      typeof item.attention !== "boolean" ||
      typeof item.fenced !== "boolean" ||
      (item.workspace_id !== undefined && !wireId(item.workspace_id)) ||
      (item.execution_location !== undefined &&
        item.execution_location !== "machine" &&
        item.execution_location !== "sandbox")
    ) {
      return undefined;
    }
    children.push({
      id: item.id,
      ...(item.title !== undefined ? { title: item.title } : {}),
      status: item.status as NonNullable<
        CodeSessionSnapshot["children"]
      >[number]["status"],
      attention: item.attention,
      fenced: item.fenced,
      ...(item.workspace_id !== undefined
        ? { workspace_id: item.workspace_id }
        : {}),
      ...(item.execution_location !== undefined
        ? { execution_location: item.execution_location }
        : {}),
    });
  }
  return children;
}

export function parseSessionTreeWait(
  value: unknown,
): CodeSessionSnapshot["wait"] | undefined {
  if (value === undefined || value === null) return null;
  if (
    !isRecord(value) ||
    !onlyKeys(value, ["waiting", "total"]) ||
    !isNonNegativeInteger(value.waiting) ||
    !isNonNegativeInteger(value.total) ||
    value.waiting > value.total ||
    value.total > 4_294_967_295
  ) {
    return undefined;
  }
  return { waiting: value.waiting, total: value.total };
}

export function parseCodeSession(value: unknown): CodeSessionSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeSessionSnapshot>(value, [
      "id",
      "access",
      "is_owner",
      "workspace_id",
      "kind",
      "harness_kind",
      "harness_version",
      "harness_resume_ref",
      "permission_mode",
      "model",
      "reasoning_effort",
      "fast_mode",
      "lifecycle",
      "fence_reason",
      "attention",
      "unrecognized_event_count",
      "visibility",
      "created_at",
      "external_origin",
      "external_origins",
      "execution_location",
      "owner_kind",
      "acts_as",
      "inference_resolutions",
      "children",
      "wait",
    ]) ||
    (value.access !== undefined &&
      value.access !== "view" &&
      value.access !== "contribute") ||
    (value.is_owner !== undefined && typeof value.is_owner !== "boolean") ||
    !wireId(value.id) ||
    // Absent for a person's session; a fixed token for a service's.
    !optionalLine(value.owner_kind) ||
    !nullableWireId(value.workspace_id) ||
    !isMember(value.kind, SESSION_KINDS) ||
    // Absent from a server before decision 0088; present as a fixed token
    // since.
    (value.execution_location !== undefined &&
      !isMember(value.execution_location, EXECUTION_LOCATIONS)) ||
    (value.acts_as !== undefined && !isMember(value.acts_as, ACTS_AS)) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    !optionalLine(value.harness_version) ||
    !optionalLine(value.harness_resume_ref) ||
    !optionalLine(value.model) ||
    (value.reasoning_effort !== undefined &&
      !isMember(value.reasoning_effort, REASONING_EFFORTS)) ||
    // Serialized unconditionally, but tolerate its absence: a session row
    // written before fast mode existed reads as off, which is what it was.
    (value.fast_mode !== undefined && typeof value.fast_mode !== "boolean") ||
    !isMember(value.permission_mode, PERMISSION_MODES) ||
    !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
    !isFiniteNumber(value.unrecognized_event_count) ||
    // Serialized unconditionally, but tolerate its absence: a session row
    // written before sharing existed is private, which is what it was
    // (decision 0086).
    (value.visibility !== undefined &&
      !isMember(value.visibility, SESSION_VISIBILITIES)) ||
    !timestamp(value.created_at)
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
  if (
    value.external_origin !== undefined &&
    (!isRecord(value.external_origin) ||
      !onlyKeys<CodeSessionExternalOrigin>(value.external_origin, [
        "channel_kind",
        "external_key",
      ]) ||
      !nonEmptyLine(value.external_origin.channel_kind) ||
      !wireId(value.external_origin.external_key))
  ) {
    return null;
  }
  const external_origin: CodeSessionExternalOrigin | undefined =
    value.external_origin === undefined
      ? undefined
      : {
          channel_kind: String(
            (value.external_origin as Record<string, unknown>).channel_kind,
          ),
          external_key: String(
            (value.external_origin as Record<string, unknown>).external_key,
          ),
        };
  let external_origins: CodeSessionExternalOrigin[] | undefined;
  if (value.external_origins !== undefined) {
    if (!Array.isArray(value.external_origins)) return null;
    external_origins = [];
    const seen = new Set<string>();
    for (const origin of value.external_origins) {
      if (
        !isRecord(origin) ||
        !onlyKeys<CodeSessionExternalOrigin>(origin, [
          "channel_kind",
          "external_key",
        ]) ||
        !nonEmptyLine(origin.channel_kind) ||
        !wireId(origin.external_key)
      )
        return null;
      const key = JSON.stringify([origin.channel_kind, origin.external_key]);
      if (seen.has(key)) return null;
      seen.add(key);
      external_origins.push({
        channel_kind: origin.channel_kind,
        external_key: origin.external_key,
      });
    }
  }
  let inference_resolutions: CodeSessionSnapshot["inference_resolutions"];
  if (value.inference_resolutions !== undefined) {
    if (!Array.isArray(value.inference_resolutions)) return null;
    inference_resolutions = [];
    const seen = new Set<string>();
    for (const resolution of value.inference_resolutions) {
      if (
        !isRecord(resolution) ||
        !onlyKeys<
          NonNullable<CodeSessionSnapshot["inference_resolutions"]>[number] & {
            subscription_label?: string;
          }
        >(resolution, [
          "scope_id",
          "provider",
          "source",
          "reason",
          "subscription_label",
        ]) ||
        !wireId(resolution.scope_id) ||
        !nonEmptyLine(resolution.provider) ||
        (resolution.source !== "owned_subscription" &&
          resolution.source !== "execution_default") ||
        !optionalLine(resolution.reason) ||
        !optionalLine(resolution.subscription_label)
      )
        return null;
      const key = JSON.stringify([resolution.scope_id, resolution.provider]);
      if (seen.has(key)) return null;
      seen.add(key);
      inference_resolutions.push({
        scope_id: resolution.scope_id,
        provider: resolution.provider,
        source: resolution.source,
        ...(resolution.reason !== undefined
          ? { reason: resolution.reason }
          : {}),
      });
    }
  }
  const children = parseSessionTreeChildren(value.children);
  if (value.children !== undefined && children === undefined) return null;
  const wait = parseSessionTreeWait(value.wait);
  if (value.wait !== undefined && wait === undefined && value.wait !== null)
    return null;
  return {
    id: value.id,
    ...(value.access !== undefined ? { access: value.access } : {}),
    ...(value.is_owner !== undefined ? { is_owner: value.is_owner } : {}),
    workspace_id: value.workspace_id,
    kind: value.kind,
    harness_kind: value.harness_kind,
    permission_mode: value.permission_mode,
    lifecycle: value.lifecycle,
    attention,
    unrecognized_event_count: value.unrecognized_event_count,
    visibility: value.visibility ?? "private",
    created_at: value.created_at,
    fast_mode: value.fast_mode === true,
    // A server from before decision 0088 sends no location; everything it
    // ran was on the machine.
    execution_location:
      value.execution_location === "sandbox" ? "sandbox" : "machine",
    ...(value.harness_version !== undefined
      ? { harness_version: value.harness_version }
      : {}),
    ...(value.harness_resume_ref !== undefined
      ? { harness_resume_ref: value.harness_resume_ref }
      : {}),
    ...(value.model !== undefined ? { model: value.model } : {}),
    ...(value.reasoning_effort !== undefined
      ? { reasoning_effort: value.reasoning_effort as ReasoningEffort }
      : {}),
    ...(fence_reason ? { fence_reason } : {}),
    ...(external_origin !== undefined ? { external_origin } : {}),
    ...(external_origins !== undefined ? { external_origins } : {}),
    ...(inference_resolutions !== undefined ? { inference_resolutions } : {}),
    ...(value.owner_kind !== undefined ? { owner_kind: value.owner_kind } : {}),
    ...(value.acts_as !== undefined
      ? { acts_as: value.acts_as as "person" | "bot" }
      : {}),
    ...(children !== undefined ? { children } : {}),
    ...(value.wait !== undefined ? { wait } : {}),
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

/**
 * The conversations a workspace page should offer, oldest first.
 *
 * A workspace runs several agents (record 55). The list arrives newest first,
 * but the tab strip reads left to right in the order the agents were started,
 * so the first one keeps its place and a new one appends to the right.
 * Archived workspaces include ended conversations so their transcripts stay readable.
 */
export function workspaceCodeSessions(
  sessions: readonly CodeSessionSnapshot[],
  includeEnded = false,
): CodeSessionSnapshot[] {
  return sessions
    .filter(
      (session) =>
        session.kind === "interactive" &&
        (includeEnded || session.lifecycle !== "ended"),
    )
    .sort(
      (left, right) =>
        left.created_at.localeCompare(right.created_at) ||
        left.id.localeCompare(right.id),
    );
}

// Shared with the inbox parser: the attention vocabulary is no longer
// code-private (decision 48 step 3). It stays defined here rather than moving
// so this change does not collide with the `idle` fix in flight; the move
// belongs with whichever lands second.
export function parseAttention(value: unknown): Attention | null {
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
    case "idle":
      return { type: value.type };
    case "needs_you":
      if (
        !blockText(value.prompt) ||
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
      if (!blockText(value.note)) return null;
      return { type: "manual", note: value.note };
    default:
      return null;
  }
}

export function parseFenceReason(value: unknown): FenceReason | null {
  if (!isRecord(value) || typeof value.type !== "string") return null;
  if (value.type === "orphan_alive") return { type: "orphan_alive" };
  if (value.type === "probe_ambiguous" && blockText(value.detail)) {
    return { type: "probe_ambiguous", detail: value.detail };
  }
  if (value.type === "resume_lost" && blockText(value.detail)) {
    return { type: "resume_lost", detail: value.detail };
  }
  if (
    (value.type === "incarnation_unresolved" ||
      value.type === "sandbox_lost" ||
      value.type === "terminal_flush_missing") &&
    blockText(value.detail)
  ) {
    return { type: value.type, detail: value.detail };
  }
  if (
    value.type === "repeated_turn_failures" &&
    typeof value.count === "number" &&
    blockText(value.detail)
  ) {
    return {
      type: "repeated_turn_failures",
      count: value.count,
      detail: value.detail,
    };
  }
  return null;
}
