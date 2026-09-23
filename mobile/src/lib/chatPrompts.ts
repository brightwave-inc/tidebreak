import {
  RENDERER_TOOL_NAMES,
  type ApprovalGrantRung,
  type NetworkPolicy,
  type PendingApprovalSnapshot as WirePendingApprovalSnapshot,
  type PendingPlanApproval as WirePendingPlanApproval,
  type PendingUserQuestions as WirePendingUserQuestions,
  type RendererToolName,
  type ToolActionPreview,
  type ToolApprovalKind,
  type UserQuestion as WireUserQuestion,
  type UserQuestionOption as WireUserQuestionOption,
  type UserQuestionType,
} from "../generated/wire";
import type { MachineClient } from "./machine";

type MachineJsonClient = Pick<MachineClient, "getJson" | "requestJson">;

export type MobilePendingToolApproval = {
  callId: string;
  turnId: string;
  action: RendererToolName;
  approval: ToolApprovalKind;
  class: WirePendingApprovalSnapshot["class"];
  preview: ToolActionPreview | null;
  canApprove: boolean;
  canRemember: boolean;
  grantRungs: ApprovalGrantRung[];
  autoJudgeStatus: NonNullable<
    WirePendingApprovalSnapshot["auto_judge_status"]
  > | null;
  /**
   * The request names a tool, an approval kind, or an action shape this app
   * does not know. It is still listed, so nothing pending is hidden, but it
   * can only be rejected here: the app cannot show the whole action, so it
   * cannot ask for consent to it.
   */
  unrecognized: boolean;
};

/**
 * Called for each part of a payload a parser tolerated instead of reading: a
 * key it does not know, a value it replaced with a safer one, or an item it
 * listed generically. The app passes nothing. The fixture tests pass a
 * collector and fail on any note, which is how drift between the server and
 * these parsers still fails a test without failing a user one release behind.
 */
export type WireDrift = (note: string) => void;

export type MobileUserQuestionOption = {
  id: string;
  label: string;
  description: string;
};

export type MobileUserQuestion = {
  id: string;
  header: string;
  question: string;
  options: MobileUserQuestionOption[];
  questionType: UserQuestionType;
  allowFreeForm: boolean;
};

export type MobilePendingUserQuestions = {
  callId: string;
  turnId: string;
  questions: MobileUserQuestion[];
  askedAt: string;
};

export type MobileUserQuestionAnswer = {
  questionId: string;
  selectedOptionIds: string[];
  customAnswer?: string;
};

export type MobilePendingPlanApproval = {
  callId: string;
  turnId: string;
  title: string;
  plan: string;
  proposedAt: string;
};

export type MobilePlanDecision =
  | { decision: "accept" }
  | { decision: "reject"; feedback?: string };

const APPROVABLE_KINDS = {
  search_may_share_query_and_excerpts: true,
  web_search_may_share_query: true,
  web_extract_may_fetch_url: true,
  exec_may_run_networked_command: true,
  external_mcp_may_call_server: true,
  workspace_may_modify_files: true,
  delegate_may_run_background_agent: true,
  computer_may_control_app: true,
  code_session_may_run_repository_agent: true,
  unsupported: false,
} as const satisfies Record<ToolApprovalKind, boolean>;

const APPROVAL_KINDS = {
  search_may_share_query_and_excerpts: true,
  web_search_may_share_query: true,
  web_extract_may_fetch_url: true,
  exec_may_run_networked_command: true,
  external_mcp_may_call_server: true,
  workspace_may_modify_files: true,
  delegate_may_run_background_agent: true,
  computer_may_control_app: true,
  code_session_may_run_repository_agent: true,
  unsupported: true,
} as const satisfies Record<ToolApprovalKind, true>;

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/**
 * Whether `value` carries no key outside `allowed`. Only the action preview
 * uses it: a preview with a key this app does not know may describe more of
 * the action than the app would show, so it is not rendered at all.
 */
function onlyKeys<Wire>(
  value: Record<string, unknown>,
  allowed: readonly (keyof Wire & string)[],
): boolean {
  const names = new Set<string>(allowed);
  return Object.keys(value).every((key) => names.has(key));
}

/**
 * Report the keys `value` carries outside `allowed`, and read on. Everywhere
 * but the action preview, a key a newer server added is ignored rather than
 * failing the payload. Typed on the wire type, so a key renamed in Rust still
 * fails to compile here.
 */
function noteUnknownKeys<Wire>(
  value: Record<string, unknown>,
  allowed: readonly (keyof Wire & string)[],
  where: string,
  drift: WireDrift | undefined,
): void {
  if (!drift) return;
  const names = new Set<string>(allowed);
  for (const key of Object.keys(value)) {
    if (!names.has(key)) drift(`${where}: unknown key ${key}`);
  }
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

function bounded(value: unknown, maxChars: number): value is string {
  return (
    typeof value === "string" &&
    Array.from(value).length <= maxChars &&
    !Array.from(value).some(forbiddenPreviewCharacter)
  );
}

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

function nonEmptyBounded(value: unknown, maxChars: number): value is string {
  return bounded(value, maxChars) && value.trim().length > 0;
}

function optionalBounded(
  value: unknown,
  maxChars: number,
): value is string | null {
  return value === null || nonEmptyBounded(value, maxChars);
}

function stringList(
  value: unknown,
  maxItems: number,
  maxChars: number,
): value is string[] {
  return (
    Array.isArray(value) &&
    value.length <= maxItems &&
    value.every((item) => bounded(item, maxChars))
  );
}

function narration(value: Record<string, unknown>): { summary?: string } {
  return nonEmptyBounded(value.summary, 200) ? { summary: value.summary } : {};
}

function parseNetworkPolicy(value: unknown): NetworkPolicy | null {
  const policy = record(value);
  if (!policy) return null;
  if (
    (policy.mode === "off" ||
      policy.mode === "package_managers" ||
      policy.mode === "open") &&
    onlyKeys<NetworkPolicy>(policy, ["mode"])
  ) {
    return { mode: policy.mode };
  }
  if (
    policy.mode !== "allowed_hosts" ||
    !onlyKeys<Extract<NetworkPolicy, { mode: "allowed_hosts" }>>(policy, [
      "mode",
      "allowed_hosts",
      "package_managers",
    ]) ||
    !stringList(policy.allowed_hosts, 64, 512) ||
    typeof policy.package_managers !== "boolean"
  ) {
    return null;
  }
  return {
    mode: "allowed_hosts",
    allowed_hosts: policy.allowed_hosts,
    package_managers: policy.package_managers,
  };
}

export function parseMobileToolActionPreview(
  value: unknown,
): ToolActionPreview | null {
  const preview = record(value);
  if (!preview) return null;
  if (preview.tool === "search") {
    if (
      !onlyKeys<Extract<ToolActionPreview, { tool: "search" }>>(preview, [
        "tool",
        "query",
        "summary",
      ]) ||
      !nonEmptyBounded(preview.query, 512)
    ) {
      return null;
    }
    return { tool: "search", query: preview.query, ...narration(preview) };
  }
  if (preview.tool === "web_search") {
    if (
      !onlyKeys<Extract<ToolActionPreview, { tool: "web_search" }>>(preview, [
        "tool",
        "query",
        "domains",
        "start_published_at",
        "end_published_at",
        "summary",
      ]) ||
      !nonEmptyBounded(preview.query, 512) ||
      !stringList(preview.domains, 32, 512) ||
      !optionalBounded(preview.start_published_at, 512) ||
      !optionalBounded(preview.end_published_at, 512)
    ) {
      return null;
    }
    return {
      tool: "web_search",
      query: preview.query,
      domains: preview.domains,
      start_published_at: preview.start_published_at,
      end_published_at: preview.end_published_at,
      ...narration(preview),
    };
  }
  if (preview.tool === "web_extract") {
    if (
      !onlyKeys<Extract<ToolActionPreview, { tool: "web_extract" }>>(preview, [
        "tool",
        "url",
        "summary",
      ]) ||
      !nonEmptyBounded(preview.url, 512)
    ) {
      return null;
    }
    return {
      tool: "web_extract",
      url: preview.url,
      ...narration(preview),
    };
  }
  if (preview.tool === "write_file") {
    if (
      !onlyKeys<Extract<ToolActionPreview, { tool: "write_file" }>>(preview, [
        "tool",
        "path",
        "summary",
      ]) ||
      !nonEmptyBounded(preview.path, 512)
    ) {
      return null;
    }
    return {
      tool: "write_file",
      path: preview.path,
      ...narration(preview),
    };
  }
  if (preview.tool === "delegate_agent") {
    const network = parseNetworkPolicy(preview.network);
    if (
      !onlyKeys<Extract<ToolActionPreview, { tool: "delegate_agent" }>>(
        preview,
        ["tool", "task", "network"],
      ) ||
      !nonEmptyBounded(preview.task, 512) ||
      !network
    ) {
      return null;
    }
    return { tool: "delegate_agent", task: preview.task, network };
  }
  if (preview.tool === "code_session") {
    const { operation, target, task, harness, model } = preview;
    if (
      !onlyKeys<Extract<ToolActionPreview, { tool: "code_session" }>>(preview, [
        "tool",
        "operation",
        "target",
        "task",
        "harness",
        "model",
      ]) ||
      (operation !== "create" && operation !== "continue") ||
      !nonEmptyBounded(target, 512) ||
      !nonEmptyBounded(task, 512) ||
      !(harness === null || nonEmptyBounded(harness, 512)) ||
      !(model === null || nonEmptyBounded(model, 512))
    )
      return null;
    return { tool: "code_session", operation, target, task, harness, model };
  }
  if (preview.tool !== "exec") return null;
  const files = preview.files === undefined ? [] : preview.files;
  if (
    !onlyKeys<Extract<ToolActionPreview, { tool: "exec" }>>(preview, [
      "tool",
      "command",
      "args",
      "cwd",
      "files",
      "summary",
    ]) ||
    !nonEmptyBounded(preview.command, 512) ||
    !stringList(preview.args, 32, 512) ||
    !nonEmptyBounded(preview.cwd, 512) ||
    !stringList(files, 32, 512)
  ) {
    return null;
  }
  return {
    tool: "exec",
    command: preview.command,
    args: preview.args,
    cwd: preview.cwd,
    files,
    ...narration(preview),
  };
}

function isRendererToolName(value: unknown): value is RendererToolName {
  return (
    typeof value === "string" &&
    (RENDERER_TOOL_NAMES as readonly string[]).includes(value)
  );
}

function isApprovalKind(value: unknown): value is ToolApprovalKind {
  return typeof value === "string" && Object.hasOwn(APPROVAL_KINDS, value);
}

function isRememberableKind(kind: ToolApprovalKind): boolean {
  return (
    APPROVABLE_KINDS[kind] &&
    kind !== "external_mcp_may_call_server" &&
    kind !== "code_session_may_run_repository_agent" &&
    kind !== "computer_may_control_app"
  );
}

function parseGrantRung(value: unknown): ApprovalGrantRung | null {
  if (value === "exact_action" || value === "whole_tool") return value;
  const rung = record(value);
  if (!rung || Object.keys(rung).length !== 1) return null;
  const command = record(rung.command_prefix);
  if (command && Object.keys(command).length === 1) {
    return Number.isSafeInteger(command.tokens) && Number(command.tokens) > 0
      ? { command_prefix: { tokens: Number(command.tokens) } }
      : null;
  }
  const path = record(rung.path_prefix);
  if (path && Object.keys(path).length === 1) {
    return Number.isSafeInteger(path.segments) && Number(path.segments) > 0
      ? { path_prefix: { segments: Number(path.segments) } }
      : null;
  }
  return null;
}

const APPROVAL_CLASSES = new Set<string>(["read_only", "workspace", "sensitive"]);

const AUTO_JUDGE_STATUSES = new Set<string>(["judging", "approved", "declined"]);

/**
 * Read one pending approval, or `null` when it names no call to decide.
 *
 * A request this app cannot fully read is still listed: an unknown tool,
 * approval kind, or class, or an action preview it cannot show whole, lists
 * as an {@link MobilePendingToolApproval.unrecognized} request that can only
 * be rejected. Where the server's policy booleans disagree with this app's,
 * the app takes whichever allows less.
 */
export function parseMobilePendingToolApproval(
  value: unknown,
  drift?: WireDrift,
): MobilePendingToolApproval | null {
  const approval = record(value);
  if (
    !approval ||
    !nonEmptyBounded(approval.call_id, 128) ||
    !nonEmptyBounded(approval.turn_id, 128)
  ) {
    drift?.("approval: no call or turn to decide");
    return null;
  }
  const where = `approval ${approval.call_id}`;
  noteUnknownKeys<WirePendingApprovalSnapshot>(
    approval,
    [
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
    ],
    where,
    drift,
  );
  const unreadable: string[] = [];
  const action = isRendererToolName(approval.action) ? approval.action : null;
  if (!action) unreadable.push(`tool ${String(approval.action)}`);
  const kind = isApprovalKind(approval.approval) ? approval.approval : null;
  if (!kind) unreadable.push(`approval kind ${String(approval.approval)}`);
  const approvalClass =
    typeof approval.class === "string" && APPROVAL_CLASSES.has(approval.class)
      ? (approval.class as WirePendingApprovalSnapshot["class"])
      : null;
  if (!approvalClass) unreadable.push(`class ${String(approval.class)}`);
  if (typeof approval.can_approve !== "boolean") {
    unreadable.push("can_approve");
  }
  const preview =
    approval.preview === undefined || approval.preview === null
      ? null
      : parseMobileToolActionPreview(approval.preview);
  if (approval.preview !== undefined && approval.preview !== null && !preview) {
    unreadable.push("action preview");
  }
  const autoJudgeStatus =
    typeof approval.auto_judge_status === "string" &&
    AUTO_JUDGE_STATUSES.has(approval.auto_judge_status)
      ? (approval.auto_judge_status as MobilePendingToolApproval["autoJudgeStatus"])
      : null;
  if (approval.auto_judge_status !== undefined && !autoJudgeStatus) {
    drift?.(`${where}: unknown auto_judge_status`);
  }
  if (unreadable.length > 0 || !action || !kind || !approvalClass) {
    drift?.(`${where}: listed as unrecognized (${unreadable.join(", ")})`);
    return {
      callId: approval.call_id,
      turnId: approval.turn_id,
      action: action ?? "other",
      approval: "unsupported",
      class: approvalClass ?? "sensitive",
      preview,
      canApprove: false,
      canRemember: false,
      grantRungs: [],
      autoJudgeStatus,
      unrecognized: true,
    };
  }
  const canApprove = approval.can_approve === true && APPROVABLE_KINDS[kind];
  if (approval.can_approve !== APPROVABLE_KINDS[kind]) {
    drift?.(`${where}: can_approve disagrees with this app's policy`);
  }
  const offered = Array.isArray(approval.grant_rungs)
    ? approval.grant_rungs.slice(0, 32)
    : [];
  const rungs = offered
    .map(parseGrantRung)
    .filter((rung): rung is ApprovalGrantRung => rung !== null);
  if (!Array.isArray(approval.grant_rungs) || rungs.length !== offered.length) {
    drift?.(`${where}: grant rungs this app does not read`);
  }
  const grantRungs = isRememberableKind(kind) ? rungs : [];
  const canRemember = approval.can_remember === true && grantRungs.length > 0;
  if (approval.can_remember !== grantRungs.length > 0) {
    drift?.(`${where}: can_remember disagrees with the grant rungs`);
  }
  return {
    callId: approval.call_id,
    turnId: approval.turn_id,
    action,
    approval: kind,
    class: approvalClass,
    preview,
    canApprove,
    canRemember,
    grantRungs: canRemember ? grantRungs : [],
    autoJudgeStatus,
    unrecognized: false,
  };
}

function parseQuestionOption(
  value: unknown,
  drift?: WireDrift,
): MobileUserQuestionOption | null {
  const option = record(value);
  if (option) {
    noteUnknownKeys<WireUserQuestionOption>(
      option,
      ["id", "label", "description"],
      "question option",
      drift,
    );
  }
  if (
    !option ||
    !nonEmptyBounded(option.id, 64) ||
    !nonEmptyBounded(option.label, 80) ||
    !nonEmptyBounded(option.description, 240)
  ) {
    return null;
  }
  return {
    id: option.id,
    label: option.label,
    description: option.description,
  };
}

function parseQuestion(
  value: unknown,
  drift?: WireDrift,
): MobileUserQuestion | null {
  const question = record(value);
  if (question) {
    noteUnknownKeys<WireUserQuestion>(
      question,
      [
        "id",
        "header",
        "question",
        "options",
        "question_type",
        "allow_free_form",
      ],
      "question",
      drift,
    );
  }
  if (
    !question ||
    !nonEmptyBounded(question.id, 64) ||
    !nonEmptyBounded(question.header, 32) ||
    !nonEmptyBounded(question.question, 500) ||
    !Array.isArray(question.options) ||
    question.options.length > 5 ||
    (question.question_type !== "single_select" &&
      question.question_type !== "multi_select") ||
    typeof question.allow_free_form !== "boolean" ||
    (question.options.length === 0 && !question.allow_free_form)
  ) {
    return null;
  }
  const options = question.options.map((option) =>
    parseQuestionOption(option, drift),
  );
  if (options.some((option) => option === null)) return null;
  const optionIds = (options as MobileUserQuestionOption[]).map(
    (option) => option.id,
  );
  if (new Set(optionIds).size !== optionIds.length) return null;
  return {
    id: question.id,
    header: question.header,
    question: question.question,
    options: options as MobileUserQuestionOption[],
    questionType: question.question_type,
    allowFreeForm: question.allow_free_form,
  };
}

export function parseMobilePendingUserQuestions(
  value: unknown,
  drift?: WireDrift,
): MobilePendingUserQuestions | null {
  const request = record(value);
  if (request) {
    noteUnknownKeys<WirePendingUserQuestions>(
      request,
      ["call_id", "turn_id", "questions", "asked_at"],
      "questions",
      drift,
    );
  }
  if (
    !request ||
    !nonEmptyBounded(request.call_id, 128) ||
    !nonEmptyBounded(request.turn_id, 128) ||
    !Array.isArray(request.questions) ||
    request.questions.length < 1 ||
    request.questions.length > 3 ||
    !nonEmptyBounded(request.asked_at, 64)
  ) {
    return null;
  }
  const questions = request.questions.map((question) =>
    parseQuestion(question, drift),
  );
  if (questions.some((question) => question === null)) return null;
  const questionIds = (questions as MobileUserQuestion[]).map(
    (question) => question.id,
  );
  if (new Set(questionIds).size !== questionIds.length) return null;
  return {
    callId: request.call_id,
    turnId: request.turn_id,
    questions: questions as MobileUserQuestion[],
    askedAt: request.asked_at,
  };
}

export function parseMobilePendingPlanApproval(
  value: unknown,
  drift?: WireDrift,
): MobilePendingPlanApproval | null {
  const request = record(value);
  if (request) {
    noteUnknownKeys<WirePendingPlanApproval>(
      request,
      ["call_id", "turn_id", "title", "plan", "proposed_at"],
      "plan",
      drift,
    );
  }
  if (
    !request ||
    !nonEmptyBounded(request.call_id, 128) ||
    !nonEmptyBounded(request.turn_id, 128) ||
    !nonEmptyBounded(request.title, 120) ||
    !boundedBlock(request.plan, 40_000) ||
    !request.plan.trim() ||
    !nonEmptyBounded(request.proposed_at, 64)
  ) {
    return null;
  }
  return {
    callId: request.call_id,
    turnId: request.turn_id,
    title: request.title,
    plan: request.plan,
    proposedAt: request.proposed_at,
  };
}

/**
 * A list of pending prompts.
 *
 * Questions and plans keep failing their list on an item this app cannot
 * read: there is no generic way to answer a question or accept a plan, and
 * the chat screen already says it could not check for them, with a retry.
 * Approvals do not, because an approval always has a safe generic answer.
 */
function parseStrictList<T>(
  value: unknown,
  parse: (item: unknown) => T | null,
  label: string,
  identity: (item: T) => string,
): T[] {
  if (!Array.isArray(value)) {
    throw new Error(`${label} response is not an array.`);
  }
  const parsed = value.map(parse);
  if (parsed.some((item) => item === null)) {
    throw new Error(`${label} response contains invalid data.`);
  }
  return unique(parsed as T[], label, identity);
}

function unique<T>(
  items: T[],
  label: string,
  identity: (item: T) => string,
): T[] {
  const identities = items.map(identity);
  if (new Set(identities).size !== identities.length) {
    throw new Error(`${label} response contains a duplicate call.`);
  }
  return items;
}

/**
 * The pending approvals, one card each.
 *
 * One approval this app cannot read no longer fails the list: it lists as an
 * unrecognized request the reader can reject. Only an item that names no call
 * at all is left out, because nothing could be decided about it.
 */
export function parseMobilePendingToolApprovals(
  value: unknown,
  drift?: WireDrift,
): MobilePendingToolApproval[] {
  if (!Array.isArray(value)) {
    throw new Error("Pending approval response is not an array.");
  }
  return unique(
    value
      .map((item) => parseMobilePendingToolApproval(item, drift))
      .filter((item): item is MobilePendingToolApproval => item !== null),
    "Pending approval",
    (approval) => approval.callId,
  );
}

export async function listMobilePendingToolApprovals(
  client: MachineJsonClient,
  chatId: string,
): Promise<MobilePendingToolApproval[]> {
  const approvals = parseMobilePendingToolApprovals(
    await client.getJson(`/chats/${encodeURIComponent(chatId)}/approvals`),
  );
  const turnIds = new Set(approvals.map((approval) => approval.turnId));
  if (turnIds.size > 1) {
    throw new Error("Pending approval response spans multiple turns.");
  }
  return approvals;
}

export async function decideMobileToolApproval(
  client: MachineJsonClient,
  chatId: string,
  callId: string,
  decision: { decision: "approve" } | { decision: "reject"; feedback: string },
): Promise<void> {
  const body =
    decision.decision === "approve"
      ? { decision: "approve", grant: null }
      : {
          decision: "reject",
          reason: validApprovalFeedback(decision.feedback),
        };
  await client.requestJson(
    `/chats/${encodeURIComponent(chatId)}/approvals/${encodeURIComponent(callId)}`,
    { method: "POST", body, expectedStatus: 204 },
  );
}

export async function listMobilePendingUserQuestions(
  client: MachineJsonClient,
  chatId: string,
): Promise<MobilePendingUserQuestions[]> {
  return parseStrictList(
    await client.getJson(
      `/chats/${encodeURIComponent(chatId)}/questions/pending`,
    ),
    parseMobilePendingUserQuestions,
    "Pending question",
    (request) => request.callId,
  );
}

export async function answerMobileUserQuestions(
  client: MachineJsonClient,
  chatId: string,
  callId: string,
  answers: MobileUserQuestionAnswer[],
  additionalUserContext?: string,
): Promise<void> {
  if (answers.length > 3) {
    throw new Error("You can answer at most three questions at once.");
  }
  const questionIds = new Set<string>();
  const bodyAnswers = answers.map((answer) => {
    if (
      !nonEmptyBounded(answer.questionId, 64) ||
      questionIds.has(answer.questionId) ||
      answer.selectedOptionIds.length > 5 ||
      !answer.selectedOptionIds.every((option) =>
        nonEmptyBounded(option, 64),
      ) ||
      new Set(answer.selectedOptionIds).size !== answer.selectedOptionIds.length
    ) {
      throw new Error("Question answers contain invalid data.");
    }
    questionIds.add(answer.questionId);
    const customAnswer = answer.customAnswer?.trim();
    if (
      customAnswer !== undefined &&
      (!boundedBlock(customAnswer, 2_000) || !customAnswer)
    ) {
      throw new Error("Custom answers must contain readable text.");
    }
    if (answer.selectedOptionIds.length === 0 && !customAnswer) {
      throw new Error("Each submitted answer must include a choice or text.");
    }
    return {
      question_id: answer.questionId,
      selected_option_ids: answer.selectedOptionIds,
      ...(customAnswer ? { custom_answer: customAnswer } : {}),
    };
  });
  const context = additionalUserContext?.trim();
  if (context !== undefined && (!boundedBlock(context, 2_000) || !context)) {
    throw new Error("Additional context must contain readable text.");
  }
  await client.requestJson(
    `/chats/${encodeURIComponent(chatId)}/questions/${encodeURIComponent(callId)}/answer`,
    {
      method: "POST",
      body: {
        answers: bodyAnswers,
        ...(context ? { additional_user_context: context } : {}),
      },
    },
  );
}

export async function listMobilePendingPlanApprovals(
  client: MachineJsonClient,
  chatId: string,
): Promise<MobilePendingPlanApproval[]> {
  return parseStrictList(
    await client.getJson(`/chats/${encodeURIComponent(chatId)}/plans/pending`),
    parseMobilePendingPlanApproval,
    "Pending plan",
    (request) => request.callId,
  );
}

export async function decideMobilePlan(
  client: MachineJsonClient,
  chatId: string,
  callId: string,
  decision: MobilePlanDecision,
): Promise<void> {
  const feedback =
    decision.decision === "reject" ? decision.feedback?.trim() : undefined;
  if (feedback !== undefined && (!boundedBlock(feedback, 4_000) || !feedback)) {
    throw new Error("Plan feedback must contain readable text.");
  }
  await client.requestJson(
    `/chats/${encodeURIComponent(chatId)}/plans/${encodeURIComponent(callId)}/decision`,
    {
      method: "POST",
      body:
        decision.decision === "reject" && feedback
          ? { decision: "reject", feedback }
          : { decision: decision.decision },
    },
  );
}

function validApprovalFeedback(value: string): string {
  const feedback = value.trim();
  if (!nonEmptyBounded(feedback, 512)) {
    throw new Error("Tell the agent what to change before rejecting.");
  }
  return feedback;
}

export function mobileApprovalQuestion(
  approval: MobilePendingToolApproval,
): string {
  if (approval.unrecognized) return "Reject this request?";
  if (!approval.canApprove) return "Reject this unsupported action?";
  switch (approval.approval) {
    case "search_may_share_query_and_excerpts":
      return "Search this conversation's sources?";
    case "web_search_may_share_query":
      return "Send this query to the web search provider?";
    case "web_extract_may_fetch_url":
      return "Fetch this public web page?";
    case "exec_may_run_networked_command":
      return "Run this command with network access?";
    case "external_mcp_may_call_server":
      return "Call this external MCP server?";
    case "workspace_may_modify_files":
      return approval.preview?.tool === "write_file"
        ? "Write this file?"
        : "Modify this work's files?";
    case "delegate_may_run_background_agent":
      return "Start this background agent?";
    case "computer_may_control_app":
      return "Control this app?";
    case "code_session_may_run_repository_agent":
      return "Start or continue this repository work?";
    case "unsupported":
      return "Reject this unsupported action?";
  }
}

/** The sentence under the question: what approving would let happen. */
export function mobileApprovalDetail(
  approval: MobilePendingToolApproval,
): string {
  return approval.unrecognized
    ? "This app does not recognize this request, so it cannot approve it. Update the app to approve it here, or reject it."
    : mobileApprovalSummary(approval.approval);
}

export function mobileApprovalSummary(kind: ToolApprovalKind): string {
  switch (kind) {
    case "search_may_share_query_and_excerpts":
      return "The search may use your query and matching excerpts from this conversation.";
    case "web_search_may_share_query":
      return "The configured provider receives the full query and filters.";
    case "web_extract_may_fetch_url":
      return "Tidebreak sends a request to the exact public URL shown here.";
    case "exec_may_run_networked_command":
      return "The command can reach the network under this conversation's policy.";
    case "external_mcp_may_call_server":
      return "The external server receives the approved call.";
    case "workspace_may_modify_files":
      return "The action can change files in this conversation's private workspace.";
    case "delegate_may_run_background_agent":
      return "The background agent runs unattended in its own workspace.";
    case "computer_may_control_app":
      return "The action can interact with the selected application.";
    case "code_session_may_run_repository_agent":
      return "The request can start or continue repository work under this conversation's permissions.";
    case "unsupported":
      return "This action cannot be approved from the mobile app.";
  }
}

export function mobileToolPreviewDetail(preview: ToolActionPreview): string {
  if (preview.tool === "code_session") {
    return [
      `${preview.operation === "create" ? "Repository" : "Session"}: ${preview.target}`,
      preview.task,
      preview.harness && `Harness: ${preview.harness}`,
      preview.model && `Model: ${preview.model}`,
    ]
      .filter(Boolean)
      .join("\n");
  }
  if (preview.tool === "search") {
    return `${preview.query}\n# searched against this conversation's sources`;
  }
  if (preview.tool === "web_search") {
    const dates =
      preview.start_published_at && preview.end_published_at
        ? `# published between ${preview.start_published_at} and ${preview.end_published_at}`
        : preview.start_published_at
          ? `# published on or after ${preview.start_published_at}`
          : preview.end_published_at
            ? `# published on or before ${preview.end_published_at}`
            : null;
    return [
      preview.query,
      preview.domains.length > 0
        ? `# limited to ${preview.domains.join(", ")}`
        : null,
      dates,
      "# sent to the configured web search provider",
    ]
      .filter((line): line is string => line !== null)
      .join("\n");
  }
  if (preview.tool === "web_extract") {
    return `${preview.url}\n# fetched from the public web`;
  }
  if (preview.tool === "write_file") {
    return `${preview.path}\n# written into this work's workspace`;
  }
  if (preview.tool === "delegate_agent") {
    const network =
      preview.network.mode === "allowed_hosts"
        ? `Allowed hosts: ${preview.network.allowed_hosts.join(", ") || "none"}${
            preview.network.package_managers ? "; package managers allowed" : ""
          }`
        : preview.network.mode === "package_managers"
          ? "Package managers only"
          : preview.network.mode === "open"
            ? "Open public network access"
            : "Network off";
    return [
      preview.task,
      `# network: ${network}`,
      "# runs unattended; its own calls are not asked about",
    ].join("\n");
  }
  const command = [preview.command, ...preview.args]
    .map(quoteArgument)
    .join(" ");
  return [
    command,
    preview.cwd !== "." ? `# working directory: ${preview.cwd}` : null,
    preview.files.length > 0
      ? `# staged files: ${preview.files.join(", ")}`
      : null,
  ]
    .filter((line): line is string => line !== null)
    .join("\n");
}

function quoteArgument(value: string): string {
  if (value.length === 0) return "''";
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(value)) return value;
  return `'${value.replaceAll("'", `'\\''`)}'`;
}
