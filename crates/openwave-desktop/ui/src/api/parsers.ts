import {
  RENDERER_TOOL_NAMES,
  type AgentActivityDetail,
  type AgentActivityKind,
  type AgentActivityOutcome,
  type PendingApprovalSnapshot,
  type ToolResultPreview as WireToolResultPreview,
  type PendingFolderAccessRequest as WirePendingFolderAccessRequest,
  type PendingOutputWritebackRequest as WirePendingOutputWritebackRequest,
  type PendingPlanApproval as WirePendingPlanApproval,
  type PendingUserQuestions as WirePendingUserQuestions,
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
  type InboxItem,
  type InboxItemKind,
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
  type SandboxAgentCancellation,
  type TaskPlan,
  type TaskPlanStep,
  type ToolActionPreview,
  type ToolResultPreview,
  type UserQuestion,
  type UserQuestionOption,
} from "./types";

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
      "claimed",
    ]) ||
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    typeof value.claimed !== "boolean"
  ) {
    return null;
  }
  return {
    callId: value.call_id,
    turnId: value.turn_id,
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
      chat_id: string;
      chat_title?: string;
      turn_id: string;
      call_id: string;
      kind: InboxItemKind;
      action?: RendererToolName;
      requested_at: string;
    }>(value, [
      "chat_id",
      "chat_title",
      "turn_id",
      "call_id",
      "kind",
      "action",
      "requested_at",
    ]) ||
    !nonEmptyBounded(value.chat_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.requested_at, 64) ||
    typeof value.kind !== "string" ||
    !INBOX_ITEM_KINDS.has(value.kind as InboxItemKind)
  ) {
    return null;
  }
  // Both are optional on the wire, and neither may arrive as anything but its
  // own declared shape — an untitled chat omits the key rather than sending an
  // empty title, and only the closed tool vocabulary may name an action.
  if (
    value.chat_title !== undefined &&
    !nonEmptyBounded(value.chat_title, 256)
  ) {
    return null;
  }
  if (
    value.action !== undefined &&
    !RENDERER_TOOL_NAMES.includes(value.action as RendererToolName)
  ) {
    return null;
  }
  return {
    chatId: value.chat_id,
    chatTitle: (value.chat_title as string | undefined) ?? null,
    turnId: value.turn_id,
    callId: value.call_id,
    kind: value.kind as InboxItemKind,
    action: (value.action as RendererToolName | undefined) ?? null,
    requestedAt: value.requested_at,
  };
}

export function parsePendingChatPrompt(value: unknown): PendingChatPrompt | null {
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
    !nonEmptyBounded(value.chat_id, 128)
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
    if (!nonEmptyBounded(callId, 128) || callIds.has(callId)) return null;
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
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    !nonEmptyBounded(value.title, 120) ||
    typeof value.plan !== "string" ||
    !value.plan.trim() ||
    Array.from(value.plan).length > 40_000 ||
    !nonEmptyBounded(value.proposed_at, 64)
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
 *
 * All or nothing, because a plan is written as one replacement and read as one
 * checklist: dropping a step that failed validation would leave a list that
 * silently disagrees with the work the agent thinks it is doing, which is a
 * worse answer than showing no plan at all.
 */
export function parseTaskPlan(value: unknown): TaskPlan | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireTaskPlan>(value, ["turn_id", "steps", "updated_at"]) ||
    !nonEmptyBounded(value.turn_id, 128) ||
    !nonEmptyBounded(value.updated_at, 64) ||
    !Array.isArray(value.steps) ||
    value.steps.length === 0 ||
    value.steps.length > MAX_TASK_PLAN_STEPS
  ) {
    return null;
  }
  const steps: TaskPlanStep[] = [];
  for (const step of value.steps) {
    if (
      !isRecord(step) ||
      !onlyKeys<WireTaskPlanStep>(step, ["content", "status"]) ||
      !nonEmptyBounded(step.content, MAX_TASK_PLAN_STEP_CHARS) ||
      typeof step.status !== "string" ||
      !TASK_PLAN_STEP_STATUSES.has(step.status as TaskPlanStepStatus)
    ) {
      return null;
    }
    steps.push({
      content: step.content,
      status: step.status as TaskPlanStepStatus,
    });
  }
  return {
    turn_id: value.turn_id,
    steps,
    updated_at: value.updated_at,
  };
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
    !nonEmptyBounded(value.call_id, 128) ||
    !nonEmptyBounded(value.turn_id, 128) ||
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
        !onlyKeys<WireUserQuestionOption>(option, ["id", "label", "description"]) ||
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

/**
 * Whether `value` carries no key outside `allowed`.
 *
 * Generic over the wire type so the allowlist has to be spelled with that type's
 * own keys: a field renamed in Rust drops out of `keyof` and the call below
 * fails to compile. Without that, a rename left the allowlist naming the old key
 * and rejecting the new one, so the validator would reject every payload and the
 * surface would simply stop appearing — with nothing failing.
 */
function onlyKeys<Wire>(
  value: Record<string, unknown>,
  allowed: readonly (keyof Wire & string)[],
): boolean {
  const set = new Set<string>(allowed);
  return Object.keys(value).every((key) => set.has(key));
}

function nonEmptyBounded(value: unknown, maxChars: number): value is string {
  return bounded(value, maxChars) && value.trim().length > 0;
}

/**
 * A string within `maxChars` characters and free of any character that could
 * break the one line it is rendered on or spoof its visual order: C0/C1
 * controls, the line and paragraph separators, and the bidirectional
 * overrides and isolates. This mirrors the projection's own clamp
 * (`preview_formatting_character` in `openwave-core`), because the renderer
 * validates what it is about to draw rather than trusting that the sender
 * already did.
 *
 * Unlike {@link nonEmptyBounded} an empty string passes. Nothing on this wire
 * is expected to be empty — the projection drops a field that clamps away —
 * so this only avoids rejecting a whole payload over a field whose emptiness
 * says nothing about its trustworthiness.
 */
function bounded(value: unknown, maxChars: number): value is string {
  return (
    typeof value === "string" &&
    Array.from(value).length <= maxChars &&
    !Array.from(value).some(forbiddenPreviewCharacter)
  );
}

/**
 * The same clamp for a field that is drawn as a block rather than a line, so
 * line breaks and tabs are structure rather than spoofing. Everything else
 * {@link bounded} rejects is still rejected: an escape sequence or a
 * bidirectional override in a pane of command output could still redraw or
 * reorder what the reader sees.
 */
function boundedBlock(value: unknown, maxChars: number): value is string {
  return (
    typeof value === "string" &&
    Array.from(value).length <= maxChars &&
    !Array.from(value).some(
      (character) =>
        forbiddenPreviewCharacter(character) &&
        character !== "\n" &&
        character !== "\t",
    )
  );
}

function forbiddenPreviewCharacter(character: string): boolean {
  const code = character.codePointAt(0) ?? 0;
  return (
    code < 32 ||
    (code >= 127 && code <= 159) ||
    code === 0x2028 ||
    code === 0x2029 ||
    (code >= 0x202a && code <= 0x202e) ||
    (code >= 0x2066 && code <= 0x2069)
  );
}

const AGENT_ACTIVITY_KINDS = new Set<AgentActivityKind>([
  "exec",
  "web_search",
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
      typeof entry.kind !== "string" ||
      !AGENT_ACTIVITY_KINDS.has(entry.kind as AgentActivityKind) ||
      typeof entry.outcome !== "string" ||
      !AGENT_ACTIVITY_OUTCOMES.has(entry.outcome as AgentActivityOutcome) ||
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
    value.can_remember !== (value.grant_rungs.length > 0) ||
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
    return { tool: "search", query };
  }
  if (value.tool === "web_search") {
    const { query, domains, start_published_at, end_published_at } = value;
    if (
      typeof query !== "string" ||
      query.length === 0 ||
      !Array.isArray(domains) ||
      !domains.every((domain): domain is string => typeof domain === "string") ||
      !isOptionalString(start_published_at) ||
      !isOptionalString(end_published_at)
    ) {
      return null;
    }
    return {
      tool: "web_search",
      query,
      domains,
      start_published_at,
      end_published_at,
    };
  }
  if (value.tool === "web_extract") {
    const { url } = value;
    if (typeof url !== "string" || url.length === 0) return null;
    return { tool: "web_extract", url };
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
  return { tool: "exec", command, args, cwd, files: staged };
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
    !isOptionalString(detail) ||
    !isOptionalString(meta) ||
    !isOptionalString(mediaType) ||
    !isOptionalString(targetId)
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
 * Whether a projected address may be handed to the host's external opener.
 *
 * The server admits only `http` and `https` into the projection, and this
 * repeats the check on the way out: the renderer is the last thing standing
 * between stored text and a browser window.
 */
function isWebUrl(value: unknown): value is string {
  if (typeof value !== "string") return false;
  try {
    const { protocol } = new URL(value);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
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
  if (typeof error !== "string" || error.length === 0 || !isOptionalString(label)) {
    return null;
  }
  return { label, error };
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
    .map((image) => {
      if (!isRecord(image)) return null;
      const { blob_id, media_type, width, height } = image;
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
    })
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

function isRendererApprovalKind(value: unknown): value is RendererApprovalKind {
  return (
    value === "search_may_share_query_and_excerpts" ||
    value === "web_search_may_share_query" ||
    value === "web_extract_may_fetch_url" ||
    value === "exec_may_run_networked_command" ||
    value === "external_mcp_may_call_server" ||
    value === "workspace_may_modify_files" ||
    value === "unsupported"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * A field the server sends as `null` when the model did not set it.
 *
 * `undefined` is not accepted: a missing key on this surface means the payload
 * is not the shape it claims to be, which is what the validator is for.
 */
function isOptionalString(value: unknown): value is string | null {
  return value === null || (typeof value === "string" && value.length > 0);
}
