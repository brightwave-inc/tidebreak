import { parseAttention } from "../code/parsers";
import {
  MAX_WIRE_CURSOR_CHARS,
  MAX_WIRE_ID_CHARS,
  MAX_WIRE_TIMESTAMP_CHARS,
  bounded,
  boundedBlock,
  isMember,
  isRecord,
  isWebUrl,
  nonEmptyBounded,
  nullableNonEmptyString,
  onlyKeys,
} from "../lib/wireDecode";
import {
  RENDERER_TOOL_NAMES,
  type AgentActivityDetail,
  type AgentActivityKind,
  type AgentActivityOutcome,
  type AnsweredUserQuestion as WireAnsweredUserQuestion,
  type PendingApprovalSnapshot,
  type ToolResultPreview as WireToolResultPreview,
  type PendingFolderAccessRequest as WirePendingFolderAccessRequest,
  type PendingOutputWritebackRequest as WirePendingOutputWritebackRequest,
  type PendingPlanApproval as WirePendingPlanApproval,
  type PendingUserQuestions as WirePendingUserQuestions,
  type AgentRunTaskPlan as WireAgentRunTaskPlan,
  type MemoryCaps as WireMemoryCaps,
  type MemoryDigest as WireMemoryDigest,
  type MemoryLink as WireMemoryLink,
  type MemoryOrigin as WireMemoryOrigin,
  type MemoryProvenance as WireMemoryProvenance,
  type MemoryRecord as WireMemoryRecord,
  type MemoryRevision as WireMemoryRevision,
  type MemorySearchHit as WireMemorySearchHit,
  type MemorySweepRun as WireMemorySweepRun,
  type MemorySweepStatus as WireMemorySweepStatus,
  type TaskPlan as WireTaskPlan,
  type TaskPlanStep as WireTaskPlanStep,
  type TaskPlanStepStatus,
  type UserQuestion as WireUserQuestion,
  type UserQuestionOption as WireUserQuestionOption,
} from "../generated/wire";
import {
  RENDERER_FOLDER_ACCESS_REASON,
  isApprovableKind,
  isRememberableKind,
  type AgentActivityHistoryEntry,
  type AgentRunProgress,
  type AgentRunProgressEntry,
  type ApprovalGrantRung,
  type ExecBackend,
  type ExecDegradation,
  type InboxConversation,
  type InboxEntry,
  type InboxItem,
  type InboxItemKind,
  type AgentNotification,
  type AgentNotificationPage,
  type NotificationContext,
  type NotificationKind,
  type NetworkPolicy,
  type MemoryCaps,
  type MemoryDigest,
  type MemoryEvidence,
  type MemoryKind,
  type MemoryLink,
  type MemoryLinkRelation,
  type MemoryOrigin,
  type MemoryProvenance,
  type MemoryRecord,
  type MemoryRevision,
  type MemoryScope,
  type MemorySearchHit,
  type MemoryStatus,
  type MemorySweepOutcome,
  type MemorySweepRun,
  type MemorySweepStatus,
  type PendingChatPrompt,
  type PendingFolderAccessRequest,
  type PendingOutputWritebackRequest,
  type PendingPlanApproval,
  type PendingToolApproval,
  type PendingUserQuestions,
  type RendererApprovalKind,
  type RendererToolName,
  type ResultEntry,
  type ResultEntryKind,
  type ResultFailure,
  type AnsweredUserQuestion,
  type SandboxAgentCancellation,
  type AgentRunTaskPlan,
  type TaskPlan,
  type TaskPlanStep,
  type ToolActionPreview,
  type ToolResultPreview,
  type UserQuestion,
  type UserQuestionOption,
} from "./types";

const MEMORY_KINDS: ReadonlySet<MemoryKind> = new Set([
  "fact",
  "preference",
  "lesson",
  "reference",
]);

const MEMORY_STATUSES: ReadonlySet<MemoryStatus> = new Set([
  "tracking",
  "proposed",
  "active",
  "archived",
  "rejected",
]);

const MEMORY_LINK_RELATIONS: ReadonlySet<MemoryLinkRelation> = new Set([
  "related",
  "updates",
  "supersedes",
]);

const MEMORY_CAP_LEVELS = new Set(["supported", "unsupported", "unknown"]);

/** Validate the backend capability vector without trusting a wire cast. */
export function parseMemoryCaps(value: unknown): MemoryCaps | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryCaps>(value, [
      "extraction",
      "lexical_search",
      "semantic_search",
      "consolidation",
      "context_assembly",
      "revision_history",
      "verified_delete",
      "asynchronous_writes",
      "agent_editable_surfaces",
    ])
  ) {
    return null;
  }
  const caps = Object.values(value);
  if (
    caps.length !== 9 ||
    caps.some(
      (level) => typeof level !== "string" || !MEMORY_CAP_LEVELS.has(level),
    )
  ) {
    return null;
  }
  return value as unknown as MemoryCaps;
}

function parseMemoryScope(value: unknown): MemoryScope | null {
  if (!isRecord(value) || typeof value.kind !== "string") return null;
  if (value.kind === "personal") {
    return onlyKeys<{ kind: string }>(value, ["kind"])
      ? { kind: "personal" }
      : null;
  }
  if (value.kind === "repo") {
    return onlyKeys<{ kind: string; repo_id: string }>(value, [
      "kind",
      "repo_id",
    ]) && nonEmptyBounded(value.repo_id, 128)
      ? { kind: "repo", repo_id: value.repo_id }
      : null;
  }
  return null;
}

function parseMemoryEvidence(value: unknown): MemoryEvidence | null {
  if (!isRecord(value)) return null;
  if (value.kind === "message") {
    return onlyKeys<{ kind: string; message_id: string }>(value, [
      "kind",
      "message_id",
    ]) && nonEmptyBounded(value.message_id, 128)
      ? { kind: "message", message_id: value.message_id }
      : null;
  }
  if (value.kind === "event") {
    return onlyKeys<{
      kind: string;
      session_id: string;
      seq: number;
    }>(value, ["kind", "session_id", "seq"]) &&
      nonEmptyBounded(value.session_id, 128) &&
      typeof value.seq === "number" &&
      Number.isSafeInteger(value.seq) &&
      value.seq > 0
      ? { kind: "event", session_id: value.session_id, seq: value.seq }
      : null;
  }
  return null;
}

function parseMemoryOrigin(value: unknown): MemoryOrigin | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryOrigin>(value, [
      "chat_id",
      "turn_id",
      "code_session_id",
      "code_turn_id",
      "workspace_id",
    ])
  ) {
    return null;
  }
  for (const id of [
    value.chat_id,
    value.turn_id,
    value.code_session_id,
    value.code_turn_id,
    value.workspace_id,
  ] as unknown[]) {
    if (!(id === undefined || id === null || nonEmptyBounded(id, 128))) {
      return null;
    }
  }
  return {
    chat_id: typeof value.chat_id === "string" ? value.chat_id : null,
    turn_id: typeof value.turn_id === "string" ? value.turn_id : null,
    code_session_id:
      typeof value.code_session_id === "string" ? value.code_session_id : null,
    code_turn_id:
      typeof value.code_turn_id === "string" ? value.code_turn_id : null,
    workspace_id:
      typeof value.workspace_id === "string" ? value.workspace_id : null,
  };
}

function parseMemoryProvenance(value: unknown): MemoryProvenance | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryProvenance>(value, ["author", "origin", "evidence"]) ||
    typeof value.author !== "string" ||
    !["user", "model", "import"].includes(value.author) ||
    !Array.isArray(value.evidence)
  ) {
    return null;
  }
  const origin = parseMemoryOrigin(value.origin);
  if (!origin) return null;
  const evidence: MemoryEvidence[] = [];
  for (const entry of value.evidence) {
    const parsed = parseMemoryEvidence(entry);
    if (!parsed) return null;
    evidence.push(parsed);
  }
  return {
    author: value.author as MemoryProvenance["author"],
    origin,
    evidence,
  };
}

function parseMemoryLink(value: unknown): MemoryLink | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryLink>(value, ["record_id", "relation"]) ||
    !nonEmptyBounded(value.record_id, 128) ||
    typeof value.relation !== "string" ||
    !MEMORY_LINK_RELATIONS.has(value.relation as MemoryLinkRelation)
  ) {
    return null;
  }
  return {
    record_id: value.record_id,
    relation: value.relation as MemoryLinkRelation,
  };
}

/** Validate one durable record before it reaches review state. */
export function parseMemoryRecord(value: unknown): MemoryRecord | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryRecord>(value, [
      "id",
      "scope",
      "kind",
      "status",
      "title",
      "body",
      "provenance",
      "links",
      "expires_at",
      "superseded_by",
      "observation_count",
      "revision",
      "created_at",
      "updated_at",
    ]) ||
    !nonEmptyBounded(value.id, 128) ||
    typeof value.kind !== "string" ||
    !MEMORY_KINDS.has(value.kind as MemoryKind) ||
    typeof value.status !== "string" ||
    !MEMORY_STATUSES.has(value.status as MemoryStatus) ||
    !nonEmptyBounded(value.title, 160) ||
    typeof value.body !== "string" ||
    value.body.trim().length === 0 ||
    !Array.isArray(value.links) ||
    typeof value.observation_count !== "number" ||
    !Number.isSafeInteger(value.observation_count) ||
    value.observation_count < 0 ||
    typeof value.revision !== "number" ||
    !Number.isSafeInteger(value.revision) ||
    value.revision < 1 ||
    !nonEmptyBounded(value.created_at, 64) ||
    !nonEmptyBounded(value.updated_at, 64)
  ) {
    return null;
  }
  const scope = parseMemoryScope(value.scope);
  const provenance = parseMemoryProvenance(value.provenance);
  if (!scope || !provenance) return null;
  const links: MemoryLink[] = [];
  for (const link of value.links) {
    const parsed = parseMemoryLink(link);
    if (!parsed || parsed.record_id === value.id) return null;
    if (links.some((existing) => existing.record_id === parsed.record_id)) {
      return null;
    }
    links.push(parsed);
  }
  if (
    !(value.expires_at === undefined || value.expires_at === null) &&
    !nonEmptyBounded(value.expires_at, 64)
  ) {
    return null;
  }
  if (
    !(value.superseded_by === undefined || value.superseded_by === null) &&
    !nonEmptyBounded(value.superseded_by, 128)
  ) {
    return null;
  }
  if (
    value.status === "tracking" &&
    (value.observation_count === 0 || provenance.evidence.length === 0)
  ) {
    return null;
  }
  if (
    provenance.author === "model" &&
    provenance.evidence.length === 0 &&
    !links.some(
      (link) => link.relation === "supersedes" || link.relation === "updates",
    )
  ) {
    return null;
  }
  return {
    id: value.id,
    scope,
    kind: value.kind as MemoryKind,
    status: value.status as MemoryStatus,
    title: value.title,
    body: value.body,
    provenance,
    links,
    expires_at: value.expires_at ?? null,
    superseded_by: value.superseded_by ?? null,
    observation_count: value.observation_count,
    revision: value.revision,
    created_at: value.created_at,
    updated_at: value.updated_at,
  };
}

/** Validate one search hit before it reaches the manager's index. */
export function parseMemorySearchHit(value: unknown): MemorySearchHit | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemorySearchHit>(value, [
      "record_id",
      "title",
      "updated_at",
      "matching_line",
      "score",
    ]) ||
    !nonEmptyBounded(value.record_id, 128) ||
    !nonEmptyBounded(value.title, 160) ||
    !nonEmptyBounded(value.updated_at, 64) ||
    typeof value.matching_line !== "string" ||
    typeof value.score !== "number" ||
    !Number.isSafeInteger(value.score) ||
    value.score < 0
  ) {
    return null;
  }
  return {
    record_id: value.record_id,
    title: value.title,
    updated_at: value.updated_at,
    matching_line: value.matching_line,
    score: value.score,
  };
}

/** Validate the derived scope digest the manager previews. */
export function parseMemoryDigest(value: unknown): MemoryDigest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryDigest>(value, [
      "scope",
      "markdown",
      "byte_len",
      "byte_cap",
      "record_count",
    ]) ||
    typeof value.markdown !== "string" ||
    typeof value.byte_len !== "number" ||
    typeof value.byte_cap !== "number" ||
    typeof value.record_count !== "number" ||
    !Number.isSafeInteger(value.byte_len) ||
    !Number.isSafeInteger(value.byte_cap) ||
    !Number.isSafeInteger(value.record_count) ||
    value.byte_len < 0 ||
    value.byte_cap < 1 ||
    value.record_count < 0 ||
    value.byte_len > value.byte_cap
  ) {
    return null;
  }
  const scope = parseMemoryScope(value.scope);
  if (!scope) return null;
  if (new TextEncoder().encode(value.markdown).length !== value.byte_len) {
    return null;
  }
  return {
    scope,
    markdown: value.markdown,
    byte_len: value.byte_len,
    byte_cap: value.byte_cap,
    record_count: value.record_count,
  };
}

const MEMORY_SWEEP_OUTCOMES: ReadonlySet<MemorySweepOutcome> = new Set([
  "proposed",
  "declined",
  "parked",
  "unchanged",
  "owner_busy",
  "no_model",
  "rate_limited",
]);

/** Validate one completed maintenance pass. */
function parseMemorySweepRun(value: unknown): MemorySweepRun | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemorySweepRun>(value, [
      "ran_at",
      "scope",
      "outcome",
      "expired",
      "proposed",
    ]) ||
    !nonEmptyBounded(value.ran_at, 64) ||
    typeof value.outcome !== "string" ||
    !MEMORY_SWEEP_OUTCOMES.has(value.outcome as MemorySweepOutcome) ||
    typeof value.expired !== "number" ||
    !Number.isSafeInteger(value.expired) ||
    value.expired < 0 ||
    typeof value.proposed !== "number" ||
    !Number.isSafeInteger(value.proposed) ||
    value.proposed < 0
  ) {
    return null;
  }
  let scope = null;
  if (value.scope !== null && value.scope !== undefined) {
    scope = parseMemoryScope(value.scope);
    if (!scope) return null;
  }
  return {
    ran_at: value.ran_at,
    scope,
    outcome: value.outcome as MemorySweepOutcome,
    expired: value.expired,
    proposed: value.proposed,
  };
}

/** Validate the maintenance sweep's last-run answer. */
export function parseMemorySweepStatus(
  value: unknown,
): MemorySweepStatus | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemorySweepStatus>(value, ["last_run"])
  ) {
    return null;
  }
  if (value.last_run === null || value.last_run === undefined) {
    return { last_run: null };
  }
  const lastRun = parseMemorySweepRun(value.last_run);
  if (!lastRun) return null;
  return { last_run: lastRun };
}

/** Validate one immutable revision snapshot. */
export function parseMemoryRevision(value: unknown): MemoryRevision | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireMemoryRevision>(value, [
      "id",
      "record_id",
      "ordinal",
      "snapshot",
      "created_at",
    ]) ||
    !nonEmptyBounded(value.id, 128) ||
    !nonEmptyBounded(value.record_id, 128) ||
    typeof value.ordinal !== "number" ||
    !Number.isSafeInteger(value.ordinal) ||
    value.ordinal < 1 ||
    !nonEmptyBounded(value.created_at, 64)
  ) {
    return null;
  }
  const snapshot = parseMemoryRecord(value.snapshot);
  if (!snapshot || snapshot.id !== value.record_id) return null;
  return {
    id: value.id,
    record_id: value.record_id,
    ordinal: value.ordinal,
    snapshot,
    created_at: value.created_at,
  };
}

export function parseFolderAccessRequest(
  value: unknown,
): PendingFolderAccessRequest | null {
  if (!isRecord(value)) return null;
  if (
    !onlyKeys<WirePendingFolderAccessRequest>(value, [
      "call_id",
      "turn_id",
      "reason",
      "folder_hint",
      "claimed",
    ]) ||
    typeof value.call_id !== "string" ||
    value.call_id.length === 0 ||
    typeof value.turn_id !== "string" ||
    value.turn_id.length === 0 ||
    typeof value.reason !== "string" ||
    value.reason !== RENDERER_FOLDER_ACCESS_REASON ||
    typeof value.claimed !== "boolean"
  ) {
    return null;
  }
  const folderHint = value.folder_hint;
  if (
    folderHint !== null &&
    folderHint !== "documents" &&
    folderHint !== "downloads"
  ) {
    return null;
  }

  return {
    callId: value.call_id,
    turnId: value.turn_id,
    reason: value.reason,
    folderHint,
    claimedByDesktop: value.claimed,
  };
}

export function parseOutputWritebackRequest(
  value: unknown,
): PendingOutputWritebackRequest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePendingOutputWritebackRequest>(value, [
      "call_id",
      "turn_id",
      "mode",
      "claimed",
    ]) ||
    !nonEmptyBounded(value.call_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.turn_id, MAX_WIRE_ID_CHARS) ||
    (value.mode !== "create" && value.mode !== "replace") ||
    typeof value.claimed !== "boolean"
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    mode: value.mode,
    claimedByDesktop: value.claimed,
  };
}

/**
 * Validate the intentionally small parked-chat summary before it reaches
 * shared shell state. Details belong to the selected chat's recovery route,
 * never to the list indicator.
 */
const INBOX_ITEM_KINDS = new Set<InboxItemKind>([
  "tool_approval",
  "question",
  "plan_review",
  "folder_access",
  "output_writeback",
]);

export function parseInboxItem(value: unknown): InboxItem | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{
      turn_id: string;
      call_id: string;
      kind: InboxItemKind;
      action?: RendererToolName;
      requested_at: string;
    }>(value, ["turn_id", "call_id", "kind", "action", "requested_at"]) ||
    !nonEmptyBounded(value.turn_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.call_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.requested_at, MAX_WIRE_TIMESTAMP_CHARS) ||
    !isMember(value.kind, INBOX_ITEM_KINDS)
  ) {
    return null;
  }
  // Only the closed tool vocabulary may name an action.
  if (
    value.action !== undefined &&
    !RENDERER_TOOL_NAMES.includes(value.action as RendererToolName)
  ) {
    return null;
  }
  return {
    turnId: value.turn_id,
    callId: value.call_id,
    kind: value.kind as InboxItemKind,
    action: (value.action as RendererToolName | undefined) ?? null,
    requestedAt: value.requested_at,
  };
}

/** The session an inbox entry belongs to. */
function parseInboxConversation(value: unknown): InboxConversation | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{ session_id: string; workspace_id?: string }>(value, [
      "session_id",
      "workspace_id",
    ]) ||
    !nonEmptyBounded(value.session_id, MAX_WIRE_ID_CHARS) ||
    (value.workspace_id !== undefined &&
      !nonEmptyBounded(value.workspace_id, MAX_WIRE_ID_CHARS))
  ) {
    return null;
  }
  return {
    sessionId: value.session_id,
    workspaceId:
      typeof value.workspace_id === "string" ? value.workspace_id : null,
  };
}

function parseNotificationContext(value: unknown): NotificationContext | null {
  if (!isRecord(value)) return null;
  if (value.surface === "chat") {
    if (
      !onlyKeys<{ surface: string; chat_id: string }>(value, [
        "surface",
        "chat_id",
      ]) ||
      !nonEmptyBounded(value.chat_id, MAX_WIRE_ID_CHARS)
    ) {
      return null;
    }
    return { surface: "chat", chatId: value.chat_id };
  }
  if (
    value.surface === "code" &&
    onlyKeys<{ surface: string; session_id: string; workspace_id: string }>(
      value,
      ["surface", "session_id", "workspace_id"],
    ) &&
    nonEmptyBounded(value.session_id, MAX_WIRE_ID_CHARS) &&
    nonEmptyBounded(value.workspace_id, MAX_WIRE_ID_CHARS)
  ) {
    return {
      surface: "code",
      sessionId: value.session_id,
      workspaceId: value.workspace_id,
    };
  }
  return null;
}

export function parseAgentNotification(
  value: unknown,
): AgentNotification | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{
      id: unknown;
      kind: unknown;
      title: unknown;
      context: unknown;
      created_at: unknown;
      read_at?: unknown;
    }>(value, ["id", "kind", "title", "context", "created_at", "read_at"]) ||
    !nonEmptyBounded(value.id, MAX_WIRE_ID_CHARS) ||
    !isNotificationKind(value.kind) ||
    !nonEmptyBounded(value.title, 512) ||
    !nonEmptyBounded(value.created_at, MAX_WIRE_TIMESTAMP_CHARS)
  ) {
    return null;
  }
  const context = parseNotificationContext(value.context);
  if (!context) return null;
  if (
    value.read_at !== undefined &&
    !nonEmptyBounded(value.read_at, MAX_WIRE_TIMESTAMP_CHARS)
  ) {
    return null;
  }
  return {
    id: value.id,
    kind: value.kind,
    title: value.title,
    context,
    createdAt: value.created_at,
    readAt: typeof value.read_at === "string" ? value.read_at : null,
  };
}

export function parseAgentNotificationPage(
  value: unknown,
): AgentNotificationPage | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{ notifications: unknown; next_cursor?: unknown }>(value, [
      "notifications",
      "next_cursor",
    ]) ||
    !Array.isArray(value.notifications)
  ) {
    return null;
  }
  if (
    value.next_cursor !== undefined &&
    !nonEmptyBounded(value.next_cursor, MAX_WIRE_CURSOR_CHARS)
  ) {
    return null;
  }
  const notifications: AgentNotification[] = [];
  for (const raw of value.notifications) {
    const row = parseAgentNotification(raw);
    if (!row) return null;
    notifications.push(row);
  }
  return {
    notifications,
    nextCursor:
      typeof value.next_cursor === "string" ? value.next_cursor : null,
  };
}

function isNotificationKind(value: unknown): value is NotificationKind {
  return value === "agent_completed" || value === "agent_failed";
}

export function parseInboxEntry(value: unknown): InboxEntry | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{
      conversation: unknown;
      title?: string;
      attention: unknown;
      items: unknown;
      waiting_since: string;
    }>(value, [
      "conversation",
      "title",
      "attention",
      "items",
      "waiting_since",
    ]) ||
    !nonEmptyBounded(value.waiting_since, MAX_WIRE_TIMESTAMP_CHARS) ||
    !Array.isArray(value.items)
  ) {
    return null;
  }
  const conversation = parseInboxConversation(value.conversation);
  if (!conversation) return null;
  const attention = parseAttention(value.attention);
  if (!attention) return null;
  // An untitled conversation omits the key rather than sending an empty title.
  if (value.title !== undefined && !nonEmptyBounded(value.title, 256)) {
    return null;
  }
  const items: InboxItem[] = [];
  for (const raw of value.items) {
    const item = parseInboxItem(raw);
    if (!item) return null;
    items.push(item);
  }
  return {
    conversation,
    title: (value.title as string | undefined) ?? null,
    attention,
    items,
    waitingSince: value.waiting_since,
  };
}

export function parsePendingChatPrompt(
  value: unknown,
): PendingChatPrompt | null {
  if (
    !isRecord(value) ||
    !onlyKeys<{
      chat_id: string;
      question_call_ids: string[];
      plan_call_ids: string[];
      folder_access_call_ids: string[];
      output_writeback_call_ids: string[];
    }>(value, [
      "chat_id",
      "question_call_ids",
      "plan_call_ids",
      "folder_access_call_ids",
      "output_writeback_call_ids",
    ]) ||
    !nonEmptyBounded(value.chat_id, MAX_WIRE_ID_CHARS)
  ) {
    return null;
  }
  const questionCallIds = parseOpaqueCallIds(value.question_call_ids);
  const planCallIds = parseOpaqueCallIds(value.plan_call_ids);
  const folderAccessCallIds = parseOpaqueCallIds(value.folder_access_call_ids);
  const outputWritebackCallIds = parseOpaqueCallIds(
    value.output_writeback_call_ids,
  );
  if (
    !questionCallIds ||
    !planCallIds ||
    !folderAccessCallIds ||
    !outputWritebackCallIds
  ) {
    return null;
  }
  const total =
    questionCallIds.length +
    planCallIds.length +
    folderAccessCallIds.length +
    outputWritebackCallIds.length;
  if (
    total === 0 ||
    new Set([
      ...questionCallIds,
      ...planCallIds,
      ...folderAccessCallIds,
      ...outputWritebackCallIds,
    ]).size !== total
  ) {
    return null;
  }
  return {
    chatId: value.chat_id,
    questionCallIds,
    planCallIds,
    folderAccessCallIds,
    outputWritebackCallIds,
  };
}

function parseOpaqueCallIds(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.length > 64) return null;
  const callIds = new Set<string>();
  for (const callId of value) {
    if (!nonEmptyBounded(callId, MAX_WIRE_ID_CHARS) || callIds.has(callId))
      return null;
    callIds.add(callId);
  }
  return [...callIds];
}

export function parsePendingPlanApproval(
  value: unknown,
): PendingPlanApproval | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePendingPlanApproval>(value, [
      "call_id",
      "turn_id",
      "title",
      "plan",
      "proposed_at",
    ]) ||
    !nonEmptyBounded(value.call_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.turn_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.title, 120) ||
    typeof value.plan !== "string" ||
    !value.plan.trim() ||
    Array.from(value.plan).length > 40_000 ||
    !nonEmptyBounded(value.proposed_at, MAX_WIRE_TIMESTAMP_CHARS)
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    title: value.title,
    plan: value.plan,
    proposedAt: value.proposed_at,
  };
}

/** The plan's step statuses, as a closed set the renderer can switch on. */
const TASK_PLAN_STEP_STATUSES = new Set<TaskPlanStepStatus>([
  "pending",
  "in_progress",
  "completed",
]);

/** Mirrors the server's own limits on a plan it will accept. */
const MAX_TASK_PLAN_STEPS = 20;
const MAX_TASK_PLAN_STEP_CHARS = 500;

/**
 * The chat's current plan, or `null` when it has none or the payload is not
 * one the renderer will draw.
 */
export function parseTaskPlan(value: unknown): TaskPlan | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireTaskPlan>(value, ["turn_id", "steps", "updated_at"]) ||
    !nonEmptyBounded(value.turn_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.updated_at, MAX_WIRE_TIMESTAMP_CHARS)
  ) {
    return null;
  }
  const steps = parseTaskPlanSteps(value.steps);
  if (steps === null) return null;
  return {
    turn_id: value.turn_id,
    steps,
    updated_at: value.updated_at,
  };
}

/**
 * One background run's plan, or `null` when it has none or the payload is not
 * one the renderer will draw.
 *
 * The run-scoped twin of {@link parseTaskPlan}, holding the steps to the same
 * rules — they are the same model-authored text written through the same tool,
 * and the only difference is what owns the list.
 */
export function parseAgentRunTaskPlan(value: unknown): AgentRunTaskPlan | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireAgentRunTaskPlan>(value, ["run_id", "steps", "updated_at"]) ||
    !nonEmptyBounded(value.run_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.updated_at, MAX_WIRE_TIMESTAMP_CHARS)
  ) {
    return null;
  }
  const steps = parseTaskPlanSteps(value.steps);
  if (steps === null) return null;
  return {
    run_id: value.run_id,
    steps,
    updated_at: value.updated_at,
  };
}

/**
 * The steps of a plan, or `null` when any one of them fails.
 *
 * All or nothing, because a plan is written as one replacement and read as one
 * checklist: dropping a step that failed validation would leave a list that
 * silently disagrees with the work the agent thinks it is doing, which is a
 * worse answer than showing no plan at all.
 */
function parseTaskPlanSteps(value: unknown): TaskPlanStep[] | null {
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.length > MAX_TASK_PLAN_STEPS
  ) {
    return null;
  }
  const steps: TaskPlanStep[] = [];
  for (const step of value) {
    if (
      !isRecord(step) ||
      !onlyKeys<WireTaskPlanStep>(step, ["content", "status"]) ||
      !nonEmptyBounded(step.content, MAX_TASK_PLAN_STEP_CHARS) ||
      !isMember(step.status, TASK_PLAN_STEP_STATUSES)
    ) {
      return null;
    }
    steps.push({
      content: step.content,
      status: step.status as TaskPlanStepStatus,
    });
  }
  return steps;
}

export function parsePendingUserQuestions(
  value: unknown,
): PendingUserQuestions | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePendingUserQuestions>(value, [
      "call_id",
      "turn_id",
      "questions",
      "asked_at",
    ]) ||
    !nonEmptyBounded(value.call_id, MAX_WIRE_ID_CHARS) ||
    !nonEmptyBounded(value.turn_id, MAX_WIRE_ID_CHARS) ||
    typeof value.asked_at !== "string" ||
    value.asked_at.length > 64 ||
    !Array.isArray(value.questions) ||
    value.questions.length < 1 ||
    value.questions.length > 3
  ) {
    return null;
  }
  const questions: UserQuestion[] = [];
  const questionIds = new Set<string>();
  for (const item of value.questions) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireUserQuestion>(item, [
        "id",
        "header",
        "question",
        "options",
        "question_type",
        "allow_free_form",
      ]) ||
      !nonEmptyBounded(item.id, 64) ||
      questionIds.has(item.id) ||
      !nonEmptyBounded(item.header, 32) ||
      !nonEmptyBounded(item.question, 500) ||
      !Array.isArray(item.options) ||
      item.options.length > 5 ||
      (item.question_type !== "single_select" &&
        item.question_type !== "multi_select") ||
      typeof item.allow_free_form !== "boolean" ||
      (item.options.length === 0 && !item.allow_free_form)
    ) {
      return null;
    }
    questionIds.add(item.id);
    const options: UserQuestionOption[] = [];
    const optionIds = new Set<string>();
    for (const option of item.options) {
      if (
        !isRecord(option) ||
        !onlyKeys<WireUserQuestionOption>(option, [
          "id",
          "label",
          "description",
        ]) ||
        !nonEmptyBounded(option.id, 64) ||
        optionIds.has(option.id) ||
        !nonEmptyBounded(option.label, 80) ||
        !nonEmptyBounded(option.description, 240)
      ) {
        return null;
      }
      optionIds.add(option.id);
      options.push({
        id: option.id,
        label: option.label,
        description: option.description,
      });
    }
    questions.push({
      id: item.id,
      header: item.header,
      question: item.question,
      options,
      questionType: item.question_type,
      allowFreeForm: item.allow_free_form,
    });
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    questions,
    askedAt: value.asked_at,
  };
}

const AGENT_ACTIVITY_KINDS = new Set<AgentActivityKind>([
  "exec",
  "web_search",
  "update_task_plan",
  "read_delegated_file",
  "list_connected_folders",
  "list_folder",
  "read_connected_file",
  "import_connected_file",
]);

const AGENT_ACTIVITY_OUTCOMES = new Set<AgentActivityOutcome>([
  "waiting",
  "running",
  "completed",
  "failed",
  "cancelled",
]);

/**
 * The bounds one activity headline may carry: the server projects each field
 * through the same caps the approval preview uses, so a longer field or a
 * larger vector is a payload it would not have produced.
 */
const ACTIVITY_DETAIL_FIELD_CHARS = 512;
const ACTIVITY_DETAIL_ARGS = 32;

/**
 * The exec output tail's cap, with slack over the server's own 2,000-character
 * bound: the point of checking is to reject a payload orders of magnitude
 * larger than the projection can emit, not to re-derive its arithmetic.
 */
const ACTIVITY_DETAIL_OUTPUT_CHARS = 4_000;

/** The half-open range of a signed 32-bit exit code, as the wire type spells it. */
const ACTIVITY_EXIT_CODE_MIN = -(2 ** 31);
const ACTIVITY_EXIT_CODE_MAX = 2 ** 31;

/**
 * The kinds a `file` headline may accompany. The server pairs one only with the
 * delegated read today, and the cross-check is worth exactly as much as it is
 * narrow: widen this when a folder tool starts naming a file, not before.
 */
const ACTIVITY_FILE_DETAIL_KINDS = new Set<AgentActivityKind>([
  "read_delegated_file",
]);

/**
 * Validate one entry's optional headline against the entry's own kind.
 *
 * The detail is model-authored text on a surface that otherwise renders only a
 * closed vocabulary, so the check is closed on every axis: an unknown tag, an
 * extra key, an unbounded field or one carrying formatting characters, an
 * oversized argument vector, or a tag that does not belong to this activity
 * kind all yield no detail. Failing that way costs the reader a headline;
 * keeping a mismatched one would let a `search` payload describe a command
 * that ran.
 */
function parseAgentActivityDetail(
  kind: AgentActivityKind,
  value: unknown,
): AgentActivityDetail | undefined {
  if (!isRecord(value)) return undefined;
  if (value.kind === "exec") {
    if (
      kind !== "exec" ||
      !onlyKeys<Extract<AgentActivityDetail, { kind: "exec" }>>(value, [
        "kind",
        "command",
        "args",
        "summary",
        "exit_code",
        "output",
      ]) ||
      !nonEmptyBounded(value.command, ACTIVITY_DETAIL_FIELD_CHARS) ||
      !Array.isArray(value.args) ||
      value.args.length > ACTIVITY_DETAIL_ARGS
    ) {
      return undefined;
    }
    const args: string[] = [];
    for (const arg of value.args) {
      if (!bounded(arg, ACTIVITY_DETAIL_FIELD_CHARS)) return undefined;
      args.push(arg);
    }
    // The exit status is decoration next to the command itself, so an
    // unusable one costs only itself: the reader still sees what ran. The
    // captured tail is treated the same way — a payload the projection could
    // not have produced is dropped, not rendered, and not fatal to the row.
    const exitCode = activityExitCode(value.exit_code);
    const output =
      value.output !== undefined &&
      boundedBlock(value.output, ACTIVITY_DETAIL_OUTPUT_CHARS) &&
      value.output.trim().length > 0
        ? value.output
        : undefined;
    return {
      kind: "exec",
      command: value.command,
      args,
      ...narration(value),
      ...(exitCode === undefined ? {} : { exit_code: exitCode }),
      ...(output === undefined ? {} : { output }),
    };
  }
  if (value.kind === "search") {
    if (
      kind !== "web_search" ||
      !onlyKeys<Extract<AgentActivityDetail, { kind: "search" }>>(value, [
        "kind",
        "query",
      ]) ||
      !nonEmptyBounded(value.query, ACTIVITY_DETAIL_FIELD_CHARS)
    ) {
      return undefined;
    }
    return { kind: "search", query: value.query };
  }
  if (value.kind === "file") {
    if (
      !ACTIVITY_FILE_DETAIL_KINDS.has(kind) ||
      !onlyKeys<Extract<AgentActivityDetail, { kind: "file" }>>(value, [
        "kind",
        "name",
      ]) ||
      !nonEmptyBounded(value.name, ACTIVITY_DETAIL_FIELD_CHARS)
    ) {
      return undefined;
    }
    return { kind: "file", name: value.name };
  }
  return undefined;
}

/**
 * The recorded exit status, when it is one a process could actually have
 * produced: the server parses it as an `i32`, so anything fractional or outside
 * that range did not come from a receipt.
 */
function activityExitCode(value: unknown): number | undefined {
  if (
    typeof value !== "number" ||
    !Number.isInteger(value) ||
    value < ACTIVITY_EXIT_CODE_MIN ||
    value >= ACTIVITY_EXIT_CODE_MAX
  ) {
    return undefined;
  }
  return value;
}

/**
 * Keep only well-formed history entries in their server order. An entry whose
 * kind or outcome falls outside the closed vocabulary, or whose timestamp is
 * missing, is dropped rather than rendered — the same defensive discipline the
 * transcript applies to every model-influenced projection.
 */
export function parseAgentActivityHistory(
  value: unknown,
): AgentActivityHistoryEntry[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    if (
      !isRecord(entry) ||
      !isMember(entry.kind, AGENT_ACTIVITY_KINDS) ||
      !isMember(entry.outcome, AGENT_ACTIVITY_OUTCOMES) ||
      typeof entry.at !== "string" ||
      entry.at.length === 0
    ) {
      return [];
    }
    const kind = entry.kind as AgentActivityKind;
    const detail = parseAgentActivityDetail(kind, entry.detail);
    return [
      {
        kind,
        outcome: entry.outcome as AgentActivityOutcome,
        at: entry.at,
        ...(detail === undefined ? {} : { detail }),
      },
    ];
  });
}

/**
 * Keep only well-formed progress lines, in server order.
 *
 * A line without a finite sequence, non-empty text, and a timestamp is dropped
 * rather than rendered. The cursor falls back to what the caller asked for, so
 * a malformed page re-reads rather than silently skipping ahead.
 */
export function parseAgentRunProgress(
  value: unknown,
  requestedAfter = 0,
): AgentRunProgress {
  if (!isRecord(value) || !Array.isArray(value.entries)) {
    return { entries: [], nextSequence: requestedAfter };
  }
  const entries = value.entries.flatMap((entry): AgentRunProgressEntry[] => {
    if (
      !isRecord(entry) ||
      typeof entry.sequence !== "number" ||
      !Number.isFinite(entry.sequence) ||
      typeof entry.text !== "string" ||
      entry.text.length === 0 ||
      typeof entry.at !== "string" ||
      entry.at.length === 0
    ) {
      return [];
    }
    return [{ sequence: entry.sequence, text: entry.text, at: entry.at }];
  });
  const nextSequence =
    typeof value.next_sequence === "number" &&
    Number.isFinite(value.next_sequence)
      ? value.next_sequence
      : requestedAfter;
  return { entries, nextSequence };
}

export function parseSandboxAgentCancellation(
  value: unknown,
): SandboxAgentCancellation | null {
  if (!isRecord(value)) return null;
  const keys = Object.keys(value);
  if (
    keys.some((key) => key !== "id" && key !== "status") ||
    typeof value.id !== "string" ||
    value.id.length === 0 ||
    (value.status !== "cancelling" && value.status !== "cancelled")
  ) {
    return null;
  }
  return { id: value.id, status: value.status };
}

/**
 * Every key a pending approval may carry.
 *
 * `satisfies` ties this to the generated wire type, so a field renamed
 * server-side fails to compile here. It used to be eight string literals
 * compared by hand: a rename would have left them allowing the old name and
 * rejecting the new one, so the validator would reject every approval and the
 * consent prompt would simply stop appearing, with nothing failing.
 */
const PENDING_APPROVAL_KEYS = [
  "call_id",
  "turn_id",
  "action",
  "approval",
  "class",
  "preview",
  "can_approve",
  "can_remember",
  "grant_rungs",
  "auto_judge_status",
] as const satisfies readonly (keyof PendingApprovalSnapshot)[];

export function parsePendingToolApproval(
  value: unknown,
): PendingToolApproval | null {
  if (!isRecord(value)) return null;
  const keys = Object.keys(value);
  if (
    keys.some(
      (key) => !(PENDING_APPROVAL_KEYS as readonly string[]).includes(key),
    ) ||
    typeof value.call_id !== "string" ||
    value.call_id.length === 0 ||
    typeof value.turn_id !== "string" ||
    value.turn_id.length === 0 ||
    !isRendererToolName(value.action) ||
    !isRendererApprovalKind(value.approval) ||
    (value.class !== "read_only" &&
      value.class !== "workspace" &&
      value.class !== "sensitive") ||
    typeof value.can_approve !== "boolean" ||
    value.can_approve !== isApprovableKind(value.approval) ||
    typeof value.can_remember !== "boolean" ||
    !Array.isArray(value.grant_rungs) ||
    value.grant_rungs.some((rung) => parseApprovalGrantRung(rung) === null) ||
    (value.grant_rungs.length > 0 && !isRememberableKind(value.approval)) ||
    value.can_remember !== value.grant_rungs.length > 0 ||
    !(
      value.auto_judge_status === undefined ||
      value.auto_judge_status === "judging" ||
      value.auto_judge_status === "approved" ||
      value.auto_judge_status === "declined"
    )
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
    action: value.action,
    approval: value.approval,
    class: value.class,
    preview: parseToolActionPreview(value.preview),
    canApprove: value.can_approve,
    canRemember: value.can_remember,
    grantRungs: value.grant_rungs.map(
      (rung) => parseApprovalGrantRung(rung) as ApprovalGrantRung,
    ),
    autoJudgeStatus: value.auto_judge_status ?? null,
  };
}

function parseApprovalGrantRung(value: unknown): ApprovalGrantRung | null {
  if (value === "exact_action" || value === "whole_tool") return value;
  if (!isRecord(value) || Object.keys(value).length !== 1) return null;
  if (isRecord(value.command_prefix)) {
    if (Object.keys(value.command_prefix).length !== 1) return null;
    const tokens = value.command_prefix.tokens;
    return typeof tokens === "number" && Number.isInteger(tokens) && tokens > 0
      ? { command_prefix: { tokens } }
      : null;
  }
  if (isRecord(value.path_prefix)) {
    if (Object.keys(value.path_prefix).length !== 1) return null;
    const segments = value.path_prefix.segments;
    return typeof segments === "number" &&
      Number.isInteger(segments) &&
      segments > 0
      ? { path_prefix: { segments } }
      : null;
  }
  return null;
}

/**
 * Validate a preview field by field. A malformed or unrecognized preview is
 * dropped rather than partially rendered: an approval card that describes the
 * wrong action is worse than one that describes no action.
 */
export function parseToolActionPreview(
  value: unknown,
): ToolActionPreview | null {
  if (value === undefined || value === null) return null;
  if (!isRecord(value)) return null;
  if (value.tool === "search") {
    const { query } = value;
    if (typeof query !== "string" || query.length === 0) return null;
    return { tool: "search", query, ...narration(value) };
  }
  if (value.tool === "web_search") {
    const { query, domains, start_published_at, end_published_at } = value;
    if (
      typeof query !== "string" ||
      query.length === 0 ||
      !Array.isArray(domains) ||
      !domains.every(
        (domain): domain is string => typeof domain === "string",
      ) ||
      !nullableNonEmptyString(start_published_at) ||
      !nullableNonEmptyString(end_published_at)
    ) {
      return null;
    }
    return {
      tool: "web_search",
      query,
      domains,
      start_published_at,
      end_published_at,
      ...narration(value),
    };
  }
  if (value.tool === "web_extract") {
    const { url } = value;
    if (typeof url !== "string" || url.length === 0) return null;
    return { tool: "web_extract", url, ...narration(value) };
  }
  if (value.tool === "write_file") {
    // The path is the whole variant: which file the call will create or
    // replace. The content deliberately never crosses the boundary.
    const { path } = value;
    if (typeof path !== "string" || path.length === 0) return null;
    return { tool: "write_file", path, ...narration(value) };
  }
  if (value.tool === "delegate_agent") {
    // The task says what the unattended run will do; the network policy is
    // what it can do with what it learns, and is the part actually being
    // consented to. A policy that cannot be read whole drops the preview
    // rather than describing a run's reach as narrower than it is.
    const { task } = value;
    const network = parseNetworkPolicy(value.network);
    if (typeof task !== "string" || task.length === 0 || network === null) {
      return null;
    }
    return { tool: "delegate_agent", task, network };
  }
  if (value.tool !== "exec") return null;
  const { command, args, cwd, files } = value;
  // `files` joined the projection after previews were already being stored, so
  // an absent list reads as staging nothing rather than dropping the card.
  const staged = files === undefined ? [] : files;
  if (
    typeof command !== "string" ||
    command.length === 0 ||
    !Array.isArray(args) ||
    !args.every((arg): arg is string => typeof arg === "string") ||
    typeof cwd !== "string" ||
    cwd.length === 0 ||
    !Array.isArray(staged) ||
    !staged.every((file): file is string => typeof file === "string")
  ) {
    return null;
  }
  return {
    tool: "exec",
    command,
    args,
    cwd,
    files: staged,
    ...narration(value),
  };
}

/**
 * Longest narration a card will show, mirroring the server's own bound.
 *
 * A summary is the one prose field on a preview, so it is the one field a call
 * could try to flood a card with. Anything longer than the server would ever
 * send is not narration; the card falls back to the literal action instead.
 */
const MAX_SUMMARY_CHARS = 200;

/**
 * The call's own account of what it is doing, when it sent a usable one.
 *
 * Unlike every other field here a bad value costs only itself: narration is
 * decoration over an action the card can already describe, so an unreadable
 * one falls back to the literal form rather than dropping the whole preview.
 * "Unreadable" is the same standard {@link bounded} applies everywhere else —
 * prose is not exempt from the check that it cannot redraw the line it is
 * about to be rendered on.
 */
function narration(value: Record<string, unknown>): { summary?: string } {
  const { summary } = value;
  if (!bounded(summary, MAX_SUMMARY_CHARS) || summary.length === 0) {
    return {};
  }
  return { summary };
}

/**
 * The network policy a delegated run inherits, validated mode by mode.
 *
 * Every mode carries its own fields, and the named-hosts one names the exact
 * destinations the run may reach — so a policy is either read whole or not
 * accepted at all. Half of it would understate the egress being approved.
 */
function parseNetworkPolicy(value: unknown): NetworkPolicy | null {
  if (!isRecord(value)) return null;
  const { mode } = value;
  if (mode === "off" || mode === "package_managers" || mode === "open") {
    return { mode };
  }
  if (mode !== "allowed_hosts") return null;
  const { allowed_hosts, package_managers } = value;
  if (
    !Array.isArray(allowed_hosts) ||
    !allowed_hosts.every((host): host is string => typeof host === "string") ||
    typeof package_managers !== "boolean"
  ) {
    return null;
  }
  return { mode, allowed_hosts, package_managers };
}

/**
 * The wire shape this validator reads, with every value still unverified.
 *
 * Keyed on the generated type rather than on `string`, because the destructuring
 * below is the one place the snake_case wire form is written out by hand. If a
 * field is renamed in Rust, the name disappears from `keyof` and this fails to
 * compile — instead of quietly destructuring to `undefined` and dropping every
 * result preview at runtime, which no test would have caught.
 */
type UncheckedExecResult = Partial<
  Record<keyof Extract<WireToolResultPreview, { tool: "exec" }>, unknown>
>;

type UncheckedMcpAppResult = Partial<
  Record<keyof Extract<WireToolResultPreview, { tool: "mcp_app" }>, unknown>
>;

type UncheckedEntriesResult = Partial<
  Record<keyof Extract<WireToolResultPreview, { tool: "entries" }>, unknown>
>;

type UncheckedUserQuestionsResult = Partial<
  Record<
    keyof Extract<WireToolResultPreview, { tool: "user_questions" }>,
    unknown
  >
>;

type UncheckedPlanDecisionResult = Partial<
  Record<
    keyof Extract<WireToolResultPreview, { tool: "plan_decision" }>,
    unknown
  >
>;

type UncheckedScreenCaptureResult = Partial<
  Record<
    keyof Extract<WireToolResultPreview, { tool: "screen_capture" }>,
    unknown
  >
>;

/** One image reference shared by the exec and screen-capture previews. The
 * `blob_id` names the pixels in the blob store; `media_type` is the snake_case
 * variant name the Rust enum serializes to. */
function parseImageRef(value: unknown): {
  attachmentId: string;
  mediaType: string;
  width: number;
  height: number;
} | null {
  if (!isRecord(value)) return null;
  const { blob_id, media_type, width, height } = value;
  if (
    typeof blob_id !== "string" ||
    !["png", "jpeg", "webp"].includes(String(media_type)) ||
    !Number.isInteger(width) ||
    Number(width) <= 0 ||
    !Number.isInteger(height) ||
    Number(height) <= 0
  ) {
    return null;
  }
  return {
    attachmentId: blob_id,
    mediaType: media_type === "jpeg" ? "image/jpeg" : `image/${media_type}`,
    width: Number(width),
    height: Number(height),
  };
}

type UncheckedAnsweredUserQuestion = Partial<
  Record<keyof WireAnsweredUserQuestion, unknown>
>;

const RESULT_ENTRY_KINDS: readonly ResultEntryKind[] = [
  "file",
  "folder",
  "source",
  "passage",
  "link",
  "output",
  "app",
];

/**
 * Validate one listed row.
 *
 * A row is dropped rather than partially rendered, on the same terms as a whole
 * preview: a row with no label is a blank line the reader cannot interpret, and
 * an unrecognized kind would reach the icon map with nothing to draw.
 */
function parseResultEntry(value: unknown): ResultEntry | null {
  if (!isRecord(value)) return null;
  const { kind, label } = value;
  // A missing hint is faithfully the absence the row shows, so `detail` and
  // `meta` are normalized rather than validated — only a present value of the
  // wrong type would be a reason to distrust the row, and it drops that field.
  const detail = value.detail ?? null;
  const meta = value.meta ?? null;
  const mediaType = value.media_type ?? null;
  const targetId = value.target_id ?? null;
  if (
    typeof label !== "string" ||
    label.length === 0 ||
    !(RESULT_ENTRY_KINDS as readonly unknown[]).includes(kind) ||
    !nullableNonEmptyString(detail) ||
    !nullableNonEmptyString(meta) ||
    !nullableNonEmptyString(mediaType) ||
    !nullableNonEmptyString(targetId)
  ) {
    return null;
  }
  // A row survives an address it cannot vouch for; it simply does not open.
  const url = isWebUrl(value.url) ? value.url : null;
  return {
    kind: kind as ResultEntryKind,
    label,
    detail,
    meta,
    mediaType,
    targetId,
    url,
  };
}

/**
 * Validate one failure row.
 *
 * The reason is what the row exists to say, so a row without a readable one is
 * dropped — and, like a dropped entry, counted as not shown rather than
 * vanishing. A failure the card quietly omits is the worst kind of omission.
 */
function parseResultFailure(value: unknown): ResultFailure | null {
  if (!isRecord(value)) return null;
  const { error } = value;
  const label = value.label ?? null;
  if (
    typeof error !== "string" ||
    error.length === 0 ||
    !nullableNonEmptyString(label)
  ) {
    return null;
  }
  return { label, error };
}

/**
 * Validate one recap row.
 *
 * `selected` and `customAnswer` are both genuinely absent for a question the
 * reader skipped, which is a row the card still shows — so their absence is
 * normalized rather than rejected, and only a present value of the wrong type
 * fails the row.
 */
function parseAnsweredUserQuestion(
  value: unknown,
): AnsweredUserQuestion | null {
  if (!isRecord(value)) return null;
  const { question, selected, custom_answer }: UncheckedAnsweredUserQuestion =
    value;
  const labels = selected ?? [];
  const custom = custom_answer ?? null;
  if (
    typeof question !== "string" ||
    question.length === 0 ||
    !Array.isArray(labels) ||
    !labels.every((label) => typeof label === "string") ||
    !nullableNonEmptyString(custom)
  ) {
    return null;
  }
  return { question, selected: labels as string[], customAnswer: custom };
}

/**
 * Validate a result field by field, on the same terms as an action: anything
 * that cannot be fully verified is dropped rather than half-rendered.
 */
export function parseToolResultPreview(
  value: unknown,
): ToolResultPreview | null {
  if (value === undefined || value === null) return null;
  if (!isRecord(value)) return null;
  if (value.tool === "web_search_provider_required") {
    return { tool: "web_search_provider_required" };
  }
  if (value.tool === "mcp_app") {
    const { server, resource_uri }: UncheckedMcpAppResult = value;
    if (
      typeof server !== "string" ||
      server.length === 0 ||
      typeof resource_uri !== "string" ||
      !resource_uri.startsWith("ui://")
    ) {
      return null;
    }
    return { tool: "mcp_app", server, resourceUri: resource_uri };
  }
  if (value.tool === "entries") {
    const { entries, failures, elided }: UncheckedEntriesResult = value;
    if (
      !Array.isArray(entries) ||
      !Array.isArray(failures) ||
      !Number.isInteger(elided) ||
      Number(elided) < 0
    ) {
      return null;
    }
    const parsedEntries = entries
      .map(parseResultEntry)
      .filter((entry): entry is ResultEntry => entry !== null);
    const parsedFailures = failures
      .map(parseResultFailure)
      .filter((failure): failure is ResultFailure => failure !== null);
    // Rows this parser rejected are counted with the ones the server bounded
    // away, because in both cases the card is showing fewer results than the
    // call returned and has to say so.
    return {
      tool: "entries",
      entries: parsedEntries,
      failures: parsedFailures,
      elided:
        Number(elided) +
        (entries.length - parsedEntries.length) +
        (failures.length - parsedFailures.length),
    };
  }
  if (value.tool === "user_questions") {
    const { answers, additional_context }: UncheckedUserQuestionsResult = value;
    if (
      !Array.isArray(answers) ||
      !nullableNonEmptyString(additional_context ?? null)
    ) {
      return null;
    }
    const parsed = answers
      .map(parseAnsweredUserQuestion)
      .filter((answer): answer is AnsweredUserQuestion => answer !== null);
    // A recap that lost a row would misreport what the reader chose, so a
    // malformed row takes the whole card down rather than being elided.
    if (parsed.length !== answers.length) return null;
    return {
      tool: "user_questions",
      answers: parsed,
      additionalContext: (additional_context as string | undefined) ?? null,
    };
  }
  if (value.tool === "plan_decision") {
    const { title, plan, accepted, feedback }: UncheckedPlanDecisionResult =
      value;
    if (
      typeof title !== "string" ||
      typeof plan !== "string" ||
      typeof accepted !== "boolean" ||
      !nullableNonEmptyString(feedback ?? null)
    ) {
      return null;
    }
    return {
      tool: "plan_decision",
      title,
      plan,
      accepted,
      feedback: (feedback as string | undefined) ?? null,
    };
  }
  if (value.tool === "screen_capture") {
    const { image, mark_count }: UncheckedScreenCaptureResult = value;
    // A capture without its image is not a card — the screenshot is the whole
    // point, so a malformed image takes the card down rather than render an
    // empty frame.
    if (!Number.isInteger(mark_count) || Number(mark_count) < 0) return null;
    const parsedImage = parseImageRef(image);
    if (parsedImage === null) return null;
    return {
      tool: "screen_capture",
      image: parsedImage,
      markCount: Number(mark_count),
    };
  }
  if (value.tool !== "exec") return null;
  const {
    exit_code,
    timed_out,
    output_truncated,
    stdout,
    stderr,
    images,
    outputs,
    degraded,
    backend,
  }: UncheckedExecResult = value;
  const imageValues = images ?? [];
  const outputValues = outputs ?? [];
  if (!Array.isArray(outputValues)) return null;
  // Like listed entries, a malformed output row is dropped rather than
  // poisoning the whole preview: the rows are display hints, not authority.
  const parsedOutputs = outputValues
    .map(parseResultEntry)
    .filter((entry): entry is ResultEntry => entry !== null);
  if (
    (exit_code !== null && typeof exit_code !== "number") ||
    typeof timed_out !== "boolean" ||
    typeof output_truncated !== "boolean" ||
    typeof stdout !== "string" ||
    typeof stderr !== "string" ||
    !Array.isArray(imageValues)
  ) {
    return null;
  }
  const parsedImages = imageValues
    .map(parseImageRef)
    .filter((image): image is NonNullable<typeof image> => image !== null);
  if (parsedImages.length !== imageValues.length) return null;
  return {
    tool: "exec",
    exitCode: exit_code,
    timedOut: timed_out,
    outputTruncated: output_truncated,
    stdout,
    stderr,
    images: parsedImages,
    outputs: parsedOutputs,
    // Unknown to this build means unshowable, not unusable: the command's
    // output still renders, without a sentence nobody wrote copy for.
    degraded: isExecDegradation(degraded) ? degraded : undefined,
    backend: isExecBackend(backend) ? backend : undefined,
  };
}

function isExecDegradation(value: unknown): value is ExecDegradation {
  return value === "sandbox_image_unavailable";
}

function isExecBackend(value: unknown): value is ExecBackend {
  return (
    value === "local" ||
    value === "e2b" ||
    value === "daytona" ||
    value === "docker"
  );
}

/**
 * Whether a provider-supplied string is a tool name the renderer will accept.
 *
 * Still an allowlist, and still a closed one — the difference is that the list
 * is now the server's own enum rather than a copy of it maintained here. The
 * copy drifted three times: two tools reached the union with no icon, and one
 * had no historical title, so a command relabelled itself on reload.
 */
export function isRendererToolName(value: unknown): value is RendererToolName {
  return (
    typeof value === "string" &&
    (RENDERER_TOOL_NAMES as readonly string[]).includes(value)
  );
}

/**
 * Every approval kind the renderer will accept, at runtime.
 *
 * Typed as a total map over the generated union rather than written out as a
 * chain of comparisons, so a kind added server-side cannot be left out here: a
 * missing key fails to compile. The chain this replaces had already drifted —
 * `delegate_may_run_background_agent` reached the wire without reaching the
 * list, and because an unparseable row fails the whole hydration response, a
 * chat with a parked spawn approval could not be reopened at all.
 */
const RENDERER_APPROVAL_KINDS = {
  search_may_share_query_and_excerpts: true,
  web_search_may_share_query: true,
  web_extract_may_fetch_url: true,
  exec_may_run_networked_command: true,
  external_mcp_may_call_server: true,
  workspace_may_modify_files: true,
  delegate_may_run_background_agent: true,
  computer_may_control_app: true,
  unsupported: true,
} as const satisfies Record<RendererApprovalKind, true>;

function isRendererApprovalKind(value: unknown): value is RendererApprovalKind {
  return (
    typeof value === "string" && Object.hasOwn(RENDERER_APPROVAL_KINDS, value)
  );
}
