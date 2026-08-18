import type {
  Attention,
  AttentionSource,
  AttentionState,
  CapLevel,
  CodeApprovalSnapshot,
  CodeApprovalState,
  CodeEvent,
  CodePermissionMode,
  CodeRepoSnapshot,
  CodeSessionLifecycle,
  CodeSessionSnapshot,
  CodeFileChange,
  CodeTurnSnapshot,
  CodeTurnStatus,
  CodeUsage,
  CodeTerminalRead,
  CodeTerminalSnapshot,
  CodeWorkspaceDiff,
  CodeWorkspaceFiles,
  CodeWorkspacePrSnapshot,
  CodeWorkspaceSnapshot,
  CodeActionSnapshot,
  CodeCommitSnapshot,
  CodePushSnapshot,
  Diffstat,
  FileChangeKind,
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
  CodeSessionDigest,
  CodeUpdateNotice,
  CodeCloneDefaults,
  CodeCloneJobSnapshot,
  PullRequestDigest,
  QueuedCodeTurn,
} from "../api/types";
import type {
  CodeEvent as WireCodeEvent,
  CodeRepoSnapshot as WireCodeRepoSnapshot,
  CodeSessionSnapshot as WireCodeSessionSnapshot,
  CodeTurnSnapshot as WireCodeTurnSnapshot,
  QueuedCodeTurn as WireQueuedCodeTurn,
  CodeTerminalRead as WireCodeTerminalRead,
  CodeTerminalSnapshot as WireCodeTerminalSnapshot,
  CodeWorkspaceDiff as WireCodeWorkspaceDiff,
  CodeWorkspaceFiles as WireCodeWorkspaceFiles,
  CodeWorkspaceSnapshot as WireCodeWorkspaceSnapshot,
  CodeWorkspacePrSnapshot as WireCodeWorkspacePrSnapshot,
  CodeActionSnapshot as WireCodeActionSnapshot,
  CodeCommitSnapshot as WireCodeCommitSnapshot,
  CodePushSnapshot as WireCodePushSnapshot,
  CodeFileChange as WireCodeFileChange,
  Diffstat as WireDiffstat,
  PullRequestDigest as WirePullRequestDigest,
  HarnessCaps as WireHarnessCaps,
  HarnessDoctorEntry as WireHarnessDoctorEntry,
  HarnessDoctorReport as WireHarnessDoctorReport,
  QuickAction as WireQuickAction,
  SequencedCodeEventFrame as WireSequencedCodeEventFrame,
  ToolDetail as WireToolDetail,
  CodeSessionDigest as WireCodeSessionDigest,
  CodeUpdateNotice as WireCodeUpdateNotice,
  CodeCloneDefaults as WireCodeCloneDefaults,
  CodeCloneJobSnapshot as WireCodeCloneJobSnapshot,
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
const PERMISSION_MODES = new Set<CodePermissionMode>(["plan", "ask", "auto", "allow"]);
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
const FILE_CHANGE_KINDS = new Set<FileChangeKind>([
  "added",
  "modified",
  "deleted",
  "renamed",
]);
const APPROVAL_STATES = new Set<CodeApprovalState>([
  "pending",
  "approved",
  "denied",
]);
const TOOL_OUTCOMES = new Set<ToolOutcome>(["succeeded", "failed", "denied"]);
const ATTENTION_SOURCES = new Set<AttentionSource>([
  "structured",
  "heuristic",
  "lifecycle",
  "user",
]);

export function parseCodeCloneJob(value: unknown): CodeCloneJobSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCloneJobSnapshot>(value, [
      "id",
      "phase",
      "percent",
      "done",
      "error",
      "repo_id",
    ]) ||
    !nonEmpty(value.id) ||
    typeof value.phase !== "string" ||
    typeof value.done !== "boolean" ||
    (value.percent !== undefined && !isFiniteNumber(value.percent)) ||
    !optionalString(value.error) ||
    !optionalString(value.repo_id)
  ) {
    return null;
  }
  return {
    id: value.id,
    phase: value.phase,
    done: value.done,
    ...(value.percent !== undefined ? { percent: value.percent } : {}),
    ...(value.error !== undefined ? { error: value.error } : {}),
    ...(value.repo_id !== undefined ? { repo_id: value.repo_id } : {}),
  };
}

export function parseCodeCloneDefaults(
  value: unknown,
): CodeCloneDefaults | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCloneDefaults>(value, [
      "parent_dir",
      "gh_found",
      "gh_authenticated",
      "gh_remediation",
    ]) ||
    !optionalString(value.parent_dir) ||
    typeof value.gh_found !== "boolean" ||
    typeof value.gh_remediation !== "string" ||
    (value.gh_authenticated !== undefined &&
      typeof value.gh_authenticated !== "boolean")
  ) {
    return null;
  }
  return {
    gh_found: value.gh_found,
    gh_remediation: value.gh_remediation,
    ...(value.parent_dir !== undefined ? { parent_dir: value.parent_dir } : {}),
    ...(value.gh_authenticated !== undefined
      ? { gh_authenticated: value.gh_authenticated }
      : {}),
  };
}

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
  const parsed: CodeWorkspaceSnapshot = {
    id: value.id,
    repo_id: value.repo_id,
    title: value.title,
    worktree_path: value.worktree_path,
    branch_name: value.branch_name,
    base_ref: value.base_ref,
    status: value.status,
    created_at: value.created_at,
    ...(value.archived_at !== undefined ? { archived_at: value.archived_at } : {}),
  };
  if (value.pr !== undefined) {
    const pr = parsePullRequestDigest(value.pr);
    if (!pr) return null;
    parsed.pr = pr;
  }
  return parsed;
}

export function parsePullRequestDigest(
  value: unknown,
): NonNullable<CodeWorkspaceSnapshot["pr"]> | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePullRequestDigest>(value, [
      "number",
      "url",
      "state",
      "title",
      "checks_summary",
      "checks",
    ]) ||
    !isFiniteNumber(value.number) ||
    !nonEmpty(value.state) ||
    (value.url !== undefined && value.url !== null && typeof value.url !== "string") ||
    (value.title !== undefined && value.title !== null && typeof value.title !== "string") ||
    (value.checks_summary !== undefined &&
      value.checks_summary !== null &&
      typeof value.checks_summary !== "string")
  ) {
    return null;
  }
  const checks = parsePullRequestChecks(value.checks);
  if (value.checks !== undefined && !checks) return null;
  return {
    number: value.number,
    state: value.state,
    ...(value.url ? { url: value.url } : {}),
    ...(value.title ? { title: value.title } : {}),
    ...(value.checks_summary ? { checks_summary: value.checks_summary } : {}),
    ...(checks && checks.length > 0 ? { checks } : {}),
  };
}

function parsePullRequestChecks(
  value: unknown,
): PullRequestDigest["checks"] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;
  const checks: NonNullable<PullRequestDigest["checks"]> = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      typeof item.name !== "string" ||
      (item.bucket !== "pass" &&
        item.bucket !== "pending" &&
        item.bucket !== "fail") ||
      (item.detail !== undefined && typeof item.detail !== "string") ||
      (item.url !== undefined && typeof item.url !== "string")
    ) {
      return null;
    }
    checks.push({
      name: item.name,
      bucket: item.bucket,
      ...(item.detail ? { detail: item.detail } : {}),
      ...(item.url ? { url: item.url } : {}),
    });
  }
  return checks;
}

export function parseCodeWorkspacePr(
  value: unknown,
): CodeWorkspacePrSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspacePrSnapshot>(value, [
      "dirty",
      "unpushed",
      "ahead",
      "has_upstream",
      "suggested_commit_message",
      "pr",
      "gh_found",
      "gh_authenticated",
      "remediation",
    ]) ||
    typeof value.dirty !== "boolean" ||
    typeof value.unpushed !== "boolean" ||
    !isFiniteNumber(value.ahead) ||
    typeof value.has_upstream !== "boolean" ||
    typeof value.suggested_commit_message !== "string" ||
    typeof value.gh_found !== "boolean" ||
    (value.gh_authenticated !== undefined &&
      typeof value.gh_authenticated !== "boolean") ||
    typeof value.remediation !== "string"
  ) {
    return null;
  }
  const parsed: CodeWorkspacePrSnapshot = {
    dirty: value.dirty,
    unpushed: value.unpushed,
    ahead: value.ahead,
    has_upstream: value.has_upstream,
    suggested_commit_message: value.suggested_commit_message,
    gh_found: value.gh_found,
    remediation: value.remediation,
    ...(value.gh_authenticated !== undefined
      ? { gh_authenticated: value.gh_authenticated }
      : {}),
  };
  if (value.pr !== undefined) {
    const pr = parsePullRequestDigest(value.pr);
    if (!pr) return null;
    parsed.pr = pr;
  }
  return parsed;
}

export function parseCodeCommit(value: unknown): CodeCommitSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCommitSnapshot>(value, ["sha", "message", "stat"]) ||
    !nonEmpty(value.sha) ||
    typeof value.message !== "string"
  ) {
    return null;
  }
  const stat = parseDiffstat(value.stat);
  if (!stat) return null;
  return { sha: value.sha, message: value.message, stat };
}

export function parseCodePush(value: unknown): CodePushSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodePushSnapshot>(value, ["branch", "remote"]) ||
    !nonEmpty(value.branch) ||
    !nonEmpty(value.remote)
  ) {
    return null;
  }
  return { branch: value.branch, remote: value.remote };
}

export function parseCodeAction(value: unknown): CodeActionSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeActionSnapshot>(value, [
      "name",
      "success",
      "exit_code",
      "stdout",
      "stderr",
      "timed_out",
    ]) ||
    !nonEmpty(value.name) ||
    typeof value.success !== "boolean" ||
    (value.exit_code !== undefined && !isFiniteNumber(value.exit_code)) ||
    typeof value.stdout !== "string" ||
    typeof value.stderr !== "string" ||
    typeof value.timed_out !== "boolean"
  ) {
    return null;
  }
  return {
    name: value.name,
    success: value.success,
    stdout: value.stdout,
    stderr: value.stderr,
    timed_out: value.timed_out,
    ...(value.exit_code !== undefined ? { exit_code: value.exit_code } : {}),
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
      "model",
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
    !optionalString(value.model) ||
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
    ...(value.model !== undefined ? { model: value.model } : {}),
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
      "usage",
      "checkpoint_ref",
      "diffstat",
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
  const usage =
    value.usage === undefined ? undefined : parseUsage(value.usage);
  if (value.usage !== undefined && !usage) return null;
  const diffstat =
    value.diffstat === undefined ? undefined : parseDiffstat(value.diffstat);
  if (value.diffstat !== undefined && !diffstat) return null;
  return {
    id: value.id,
    session_id: value.session_id,
    ordinal: value.ordinal,
    status: value.status,
    user_input: value.user_input,
    started_at: value.started_at,
    ...(usage ? { usage } : {}),
    ...(value.checkpoint_ref !== undefined
      ? { checkpoint_ref: value.checkpoint_ref }
      : {}),
    ...(diffstat ? { diffstat } : {}),
    ...(value.ended_at !== undefined ? { ended_at: value.ended_at } : {}),
  };
}

/**
 * What `POST /code/sessions/{id}/turns` did with the message.
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
    !onlyKeys<WireQueuedCodeTurn>(value, ["session_id", "message", "position"]) ||
    !nonEmpty(value.session_id) ||
    typeof value.message !== "string" ||
    !isFiniteNumber(value.position)
  ) {
    return null;
  }
  return {
    session_id: value.session_id,
    message: value.message,
    position: value.position,
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

export function parseCodeWorkspaceFiles(
  value: unknown,
): CodeWorkspaceFiles | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceFiles>(value, [
      "files",
      "truncated",
      "stat",
      "turn_id",
    ]) ||
    !Array.isArray(value.files) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const files: CodeFileChange[] = [];
  for (const item of value.files) {
    const parsed = parseCodeFileChange(item);
    if (!parsed) return null;
    files.push(parsed);
  }
  const stat = parseDiffstat(value.stat);
  if (!stat) return null;
  if (value.turn_id !== undefined && !nonEmpty(value.turn_id)) return null;
  return {
    files,
    truncated: value.truncated,
    stat,
    ...(value.turn_id !== undefined ? { turn_id: value.turn_id } : {}),
  };
}

export function parseCodeWorkspaceDiff(
  value: unknown,
): CodeWorkspaceDiff | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceDiff>(value, [
      "diff",
      "truncated",
      "stat",
      "turn_id",
      "file",
    ]) ||
    typeof value.diff !== "string" ||
    typeof value.truncated !== "boolean" ||
    !optionalString(value.turn_id) ||
    !optionalString(value.file)
  ) {
    return null;
  }
  const stat = parseDiffstat(value.stat);
  if (!stat) return null;
  return {
    diff: value.diff,
    truncated: value.truncated,
    stat,
    ...(value.turn_id !== undefined ? { turn_id: value.turn_id } : {}),
    ...(value.file !== undefined ? { file: value.file } : {}),
  };
}

function parseCodeFileChange(value: unknown): CodeFileChange | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeFileChange>(value, [
      "path",
      "kind",
      "insertions",
      "deletions",
      "previous_path",
    ]) ||
    typeof value.path !== "string" ||
    !isMember(value.kind, FILE_CHANGE_KINDS) ||
    !isFiniteNumber(value.insertions) ||
    !isFiniteNumber(value.deletions) ||
    !optionalString(value.previous_path)
  ) {
    return null;
  }
  return {
    path: value.path,
    kind: value.kind,
    insertions: value.insertions,
    deletions: value.deletions,
    ...(value.previous_path !== undefined
      ? { previous_path: value.previous_path }
      : {}),
  };
}

export function parseDiffstat(value: unknown): Diffstat | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireDiffstat>(value, [
      "files",
      "insertions",
      "deletions",
      "truncated",
    ]) ||
    !isFiniteNumber(value.files) ||
    !isFiniteNumber(value.insertions) ||
    !isFiniteNumber(value.deletions) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  return {
    files: value.files,
    insertions: value.insertions,
    deletions: value.deletions,
    truncated: value.truncated,
  };
}

/** `GET /code/sessions/{id}/turns` — oldest first. */
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

export function parseCodeTerminal(value: unknown): CodeTerminalSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTerminalSnapshot>(value, [
      "id",
      "workspace_id",
      "cols",
      "rows",
      "ended",
      "created_at",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.workspace_id) ||
    !isFiniteNumber(value.cols) ||
    !isFiniteNumber(value.rows) ||
    typeof value.ended !== "boolean" ||
    !nonEmpty(value.created_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    cols: value.cols,
    rows: value.rows,
    ended: value.ended,
    created_at: value.created_at,
  };
}

export function parseCodeTerminalList(
  value: unknown,
): CodeTerminalSnapshot[] | null {
  if (!Array.isArray(value)) return null;
  const terminals: CodeTerminalSnapshot[] = [];
  for (const item of value) {
    const parsed = parseCodeTerminal(item);
    if (!parsed) return null;
    terminals.push(parsed);
  }
  return terminals;
}

export function parseCodeTerminalRead(value: unknown): CodeTerminalRead | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTerminalRead>(value, [
      "id",
      "workspace_id",
      "bytes",
      "cursor",
      "overflow",
      "truncated",
      "ended",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.workspace_id) ||
    typeof value.bytes !== "string" ||
    !isFiniteNumber(value.cursor) ||
    typeof value.overflow !== "boolean" ||
    typeof value.truncated !== "boolean" ||
    typeof value.ended !== "boolean"
  ) {
    return null;
  }
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    bytes: value.bytes,
    cursor: value.cursor,
    overflow: value.overflow,
    truncated: value.truncated,
    ended: value.ended,
  };
}

export function parseHarnessModelList(
  value: unknown,
): { kind: HarnessKind; models: { id: string; label: string; default: boolean }[] } | null {
  if (
    !isRecord(value) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !Array.isArray(value.models)
  ) {
    return null;
  }
  const models: { id: string; label: string; default: boolean }[] = [];
  for (const item of value.models) {
    if (
      !isRecord(item) ||
      typeof item.id !== "string" ||
      typeof item.label !== "string" ||
      typeof item.default !== "boolean"
    ) {
      return null;
    }
    models.push({ id: item.id, label: item.label, default: item.default });
  }
  return { kind: value.kind, models };
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
      "auto_mode",
      "allow_mode",
      "reasoning_levels",
      "native_file_change_events",
      "native_interrupt",
    ]) ||
    !isMember(value.resume, CAP_LEVELS) ||
    !isMember(value.streaming_deltas, CAP_LEVELS) ||
    !isMember(value.structured_approvals, CAP_LEVELS) ||
    !isMember(value.mid_turn_steering, CAP_LEVELS) ||
    !isMember(value.plan_mode, CAP_LEVELS) ||
    !isMember(value.auto_mode, CAP_LEVELS) ||
    !isMember(value.allow_mode, CAP_LEVELS) ||
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
    auto_mode: value.auto_mode,
    allow_mode: value.allow_mode,
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
    case "checkpoint_recorded": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "checkpoint_recorded" }>>(
          value,
          ["type", "turn_id", "diffstat"],
        ) ||
        !nonEmpty(value.turn_id)
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
    case "approval_requested":
      if (
        !onlyKeys(value, ["type", "approval_id"]) ||
        !nonEmpty(value.approval_id)
      ) {
        return null;
      }
      return { type: "approval_requested", approval_id: value.approval_id };
    case "approval_resolved":
      if (
        !onlyKeys(value, ["type", "approval_id", "decision"]) ||
        !nonEmpty(value.approval_id) ||
        !isRecord(value.decision) ||
        (value.decision.type !== "approve" && value.decision.type !== "deny")
      ) {
        return null;
      }
      return {
        type: "approval_resolved",
        approval_id: value.approval_id,
        decision: value.decision as Extract<
          CodeEvent,
          { type: "approval_resolved" }
        >["decision"],
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
  if (value.type === "resume_lost" && typeof value.detail === "string") {
    return { type: "resume_lost", detail: value.detail };
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

export function parseCodeSessionDigest(value: unknown): CodeSessionDigest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeSessionDigest>(value, [
      "workspace",
      "session",
      "lifecycle",
      "attention",
      "title",
      "turn_count",
      "pr_state",
    ]) ||
    !nonEmpty(value.workspace) ||
    !nonEmpty(value.session) ||
    !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
    typeof value.title !== "string" ||
    !isFiniteNumber(value.turn_count)
  ) {
    return null;
  }
  const attention = parseAttention(value.attention);
  if (!attention) return null;
  const pr_state =
    value.pr_state === undefined ? undefined : parsePrState(value.pr_state);
  if (value.pr_state !== undefined && !pr_state) return null;
  return {
    workspace: value.workspace,
    session: value.session,
    lifecycle: value.lifecycle,
    attention,
    title: value.title,
    turn_count: value.turn_count,
    ...(pr_state ? { pr_state } : {}),
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
          "lifecycle",
          "attention",
          "title",
          "turn_count",
          "pr_state",
        ]) ||
        !nonEmpty(value.workspace) ||
        !nonEmpty(value.session) ||
        !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
        typeof value.title !== "string" ||
        !isFiniteNumber(value.turn_count)
      ) {
        return null;
      }
      const attention = parseAttention(value.attention);
      if (!attention) return null;
      const pr_state =
        value.pr_state === undefined ? undefined : parsePrState(value.pr_state);
      if (value.pr_state !== undefined && !pr_state) return null;
      return {
        type: "digest",
        workspace: value.workspace,
        session: value.session,
        lifecycle: value.lifecycle,
        attention,
        title: value.title,
        turn_count: value.turn_count,
        ...(pr_state ? { pr_state } : {}),
      };
    }
    case "clone_progress": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "clone_progress" }>>(
          value,
          ["type", "job", "phase", "percent", "done", "error", "repo_id"],
        ) ||
        !nonEmpty(value.job) ||
        typeof value.phase !== "string" ||
        typeof value.done !== "boolean" ||
        (value.percent !== undefined && !isFiniteNumber(value.percent)) ||
        !optionalString(value.error) ||
        !optionalString(value.repo_id)
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
    case "terminal_activity": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "terminal_activity" }>>(
          value,
          ["type", "workspace_id", "terminal_id"],
        ) ||
        !nonEmpty(value.workspace_id) ||
        !nonEmpty(value.terminal_id)
      ) {
        return null;
      }
      return {
        type: "terminal_activity",
        workspace_id: value.workspace_id,
        terminal_id: value.terminal_id,
      };
    }
    default:
      return null;
  }
}

function parsePrState(value: unknown): PullRequestDigest | null {
  if (
    !isRecord(value) ||
    !isFiniteNumber(value.number) ||
    typeof value.state !== "string"
  ) {
    return null;
  }
  const checks = parsePullRequestChecks(value.checks);
  return {
    number: value.number,
    state: value.state,
    ...(typeof value.url === "string" ? { url: value.url } : {}),
    ...(typeof value.title === "string" ? { title: value.title } : {}),
    ...(typeof value.checks_summary === "string"
      ? { checks_summary: value.checks_summary }
      : {}),
    ...(checks && checks.length > 0 ? { checks } : {}),
  };
}

export function parseCodeApproval(value: unknown): CodeApprovalSnapshot | null {
  if (
    !isRecord(value) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.session_id) ||
    !nonEmpty(value.turn_id) ||
    !isRecord(value.kind) ||
    typeof value.kind.type !== "string" ||
    typeof value.harness_raw_json !== "string" ||
    !isMember(value.state, APPROVAL_STATES) ||
    !nonEmpty(value.requested_at) ||
    (value.feedback !== undefined && typeof value.feedback !== "string") ||
    (value.decided_at !== undefined && typeof value.decided_at !== "string")
  ) {
    return null;
  }
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
  };
}
