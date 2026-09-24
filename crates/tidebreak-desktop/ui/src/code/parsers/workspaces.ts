import {
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
  CodeWorkspacePrSnapshot,
  CodeWatchSnapshot,
  CodeWorkspaceSnapshot,
  CodeActionSnapshot,
  CodeCommitSnapshot,
  CodePushSnapshot,
  PullRequestDigest,
  PullRequestComment,
  PullRequestCommentKind,
  CodePrCommentsSnapshot,
  CodeCheckLog,
  CodeCheckLogError,
  CodeCheckLogsSnapshot,
} from "../../api/types";
import type {
  CodeWorkspaceSnapshot as WireCodeWorkspaceSnapshot,
  CodeWorkspacePrSnapshot as WireCodeWorkspacePrSnapshot,
  CodeTriggerSnapshot as WireCodeTriggerSnapshot,
  CodeWatchSnapshot as WireCodeWatchSnapshot,
  CodeActionSnapshot as WireCodeActionSnapshot,
  CodeCommitSnapshot as WireCodeCommitSnapshot,
  CodePushSnapshot as WireCodePushSnapshot,
  PullRequestDigest as WirePullRequestDigest,
  PullRequestCheckCounts as WirePullRequestCheckCounts,
  PullRequestComment as WirePullRequestComment,
  CodePrCommentsSnapshot as WireCodePrCommentsSnapshot,
  CodeCheckLog as WireCodeCheckLog,
  CodeCheckLogError as WireCodeCheckLogError,
  CodeCheckLogsSnapshot as WireCodeCheckLogsSnapshot,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  blockText,
  optionalBlock,
  rawText,
  wireId,
  optionalWireId,
  timestamp,
  optionalTimestamp,
  WATCH_STATES,
  WORKSPACE_STATUSES,
} from "./shared";
import { parseDiffstat } from "./files";

function parseCodeCheckLog(value: unknown): CodeCheckLog | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCheckLog>(value, [
      "check",
      "path",
      "byte_len",
      "truncated",
      "url",
    ]) ||
    !nonEmptyLine(value.check) ||
    !nonEmptyLine(value.path) ||
    typeof value.byte_len !== "number" ||
    typeof value.truncated !== "boolean" ||
    !lineText(value.url)
  ) {
    return null;
  }
  return {
    check: value.check,
    path: value.path,
    byte_len: value.byte_len,
    truncated: value.truncated,
    url: value.url,
  };
}

function parseCodeCheckLogError(value: unknown): CodeCheckLogError | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCheckLogError>(value, ["check", "message"]) ||
    !nonEmptyLine(value.check) ||
    !blockText(value.message)
  ) {
    return null;
  }
  return { check: value.check, message: value.message };
}

export function parseCodeCheckLogsSnapshot(
  value: unknown,
): CodeCheckLogsSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCheckLogsSnapshot>(value, [
      "head_sha",
      "logs",
      "errors",
    ]) ||
    !optionalWireId(value.head_sha) ||
    !Array.isArray(value.logs) ||
    !Array.isArray(value.errors)
  ) {
    return null;
  }
  const logs: CodeCheckLog[] = [];
  for (const entry of value.logs) {
    const log = parseCodeCheckLog(entry);
    if (!log) return null;
    logs.push(log);
  }
  const errors: CodeCheckLogError[] = [];
  for (const entry of value.errors) {
    const error = parseCodeCheckLogError(entry);
    if (!error) return null;
    errors.push(error);
  }
  return {
    ...(value.head_sha === undefined ? {} : { head_sha: value.head_sha }),
    logs,
    errors,
  };
}

export function parseCodeWorkspace(
  value: unknown,
): CodeWorkspaceSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceSnapshot>(value, [
      "base_refresh_warning",
      "read_only",
      "is_owner",
      "id",
      "repo_id",
      "repo_display_name",
      "title",
      "worktree_path",
      "branch_name",
      "base_ref",
      "status",
      "pr",
      "created_at",
      "archived_at",
      "released_at",
      "released_tip",
      "bundle_bytes",
      "setup_error",
    ]) ||
    (value.read_only !== undefined && typeof value.read_only !== "boolean") ||
    (value.is_owner !== undefined && typeof value.is_owner !== "boolean") ||
    (value.base_refresh_warning !== undefined &&
      !nonEmptyLine(value.base_refresh_warning)) ||
    !wireId(value.id) ||
    !wireId(value.repo_id) ||
    (value.repo_display_name !== undefined &&
      !nonEmptyLine(value.repo_display_name)) ||
    !nonEmptyLine(value.title) ||
    !nonEmptyLine(value.worktree_path) ||
    !nonEmptyLine(value.branch_name) ||
    !nonEmptyLine(value.base_ref) ||
    !isMember(value.status, WORKSPACE_STATUSES) ||
    !timestamp(value.created_at) ||
    !optionalTimestamp(value.archived_at) ||
    !optionalTimestamp(value.released_at) ||
    !optionalWireId(value.released_tip) ||
    (value.bundle_bytes !== undefined && !isFiniteNumber(value.bundle_bytes)) ||
    (value.setup_error !== undefined && !blockText(value.setup_error))
  ) {
    return null;
  }
  const parsed: CodeWorkspaceSnapshot = {
    ...(value.read_only !== undefined ? { read_only: value.read_only } : {}),
    ...(value.is_owner !== undefined ? { is_owner: value.is_owner } : {}),
    ...(value.base_refresh_warning !== undefined
      ? { base_refresh_warning: value.base_refresh_warning }
      : {}),
    id: value.id,
    repo_id: value.repo_id,
    ...(value.repo_display_name !== undefined
      ? { repo_display_name: value.repo_display_name }
      : {}),
    title: value.title,
    worktree_path: value.worktree_path,
    branch_name: value.branch_name,
    base_ref: value.base_ref,
    status: value.status,
    created_at: value.created_at,
    ...(value.archived_at !== undefined
      ? { archived_at: value.archived_at }
      : {}),
    ...(value.released_at !== undefined
      ? { released_at: value.released_at }
      : {}),
    ...(value.released_tip !== undefined
      ? { released_tip: value.released_tip }
      : {}),
    ...(value.bundle_bytes !== undefined
      ? { bundle_bytes: value.bundle_bytes }
      : {}),
    ...(value.setup_error !== undefined
      ? { setup_error: value.setup_error }
      : {}),
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
  const optionalStringField = (field: unknown) =>
    field === undefined || field === null || lineText(field);
  const optionalBooleanField = (field: unknown) =>
    field === undefined || field === null || typeof field === "boolean";
  if (
    !isRecord(value) ||
    !onlyKeys<WirePullRequestDigest>(value, [
      "number",
      "url",
      "state",
      "title",
      "checks_summary",
      "check_counts",
      "checks",
      "draft",
      "merged",
      "review_decision",
      "mergeable",
      "merge_state_status",
      "head_branch",
      "base_branch",
      "head_sha",
      "auto_merge_enabled",
      "in_merge_queue",
    ]) ||
    !isFiniteNumber(value.number) ||
    !nonEmptyLine(value.state) ||
    !optionalStringField(value.url) ||
    !optionalStringField(value.title) ||
    !optionalStringField(value.checks_summary) ||
    !optionalStringField(value.review_decision) ||
    !optionalStringField(value.mergeable) ||
    !optionalStringField(value.merge_state_status) ||
    !optionalStringField(value.head_branch) ||
    !optionalStringField(value.base_branch) ||
    !optionalStringField(value.head_sha) ||
    !optionalBooleanField(value.draft) ||
    !optionalBooleanField(value.merged) ||
    !optionalBooleanField(value.auto_merge_enabled) ||
    !optionalBooleanField(value.in_merge_queue)
  ) {
    return null;
  }
  const checks = parsePullRequestChecks(value.checks);
  if (value.checks !== undefined && !checks) return null;
  const counts =
    value.check_counts === undefined || value.check_counts === null
      ? undefined
      : parsePullRequestCheckCounts(value.check_counts);
  if (counts === null) return null;
  return {
    number: value.number,
    state: value.state,
    ...(value.url ? { url: value.url } : {}),
    ...(value.title ? { title: value.title } : {}),
    ...(value.checks_summary ? { checks_summary: value.checks_summary } : {}),
    ...(counts ? { check_counts: counts } : {}),
    ...(checks && checks.length > 0 ? { checks } : {}),
    ...(typeof value.draft === "boolean" ? { draft: value.draft } : {}),
    ...(typeof value.merged === "boolean" ? { merged: value.merged } : {}),
    ...(value.review_decision
      ? { review_decision: value.review_decision }
      : {}),
    ...(value.mergeable ? { mergeable: value.mergeable } : {}),
    ...(value.merge_state_status
      ? { merge_state_status: value.merge_state_status }
      : {}),
    ...(value.head_branch ? { head_branch: value.head_branch } : {}),
    ...(value.base_branch ? { base_branch: value.base_branch } : {}),
    ...(value.head_sha ? { head_sha: value.head_sha } : {}),
    ...(typeof value.auto_merge_enabled === "boolean"
      ? { auto_merge_enabled: value.auto_merge_enabled }
      : {}),
    ...(typeof value.in_merge_queue === "boolean"
      ? { in_merge_queue: value.in_merge_queue }
      : {}),
  };
}

export function parsePullRequestChecks(
  value: unknown,
): PullRequestDigest["checks"] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;
  const checks: NonNullable<PullRequestDigest["checks"]> = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !lineText(item.name) ||
      (item.bucket !== "pass" &&
        item.bucket !== "pending" &&
        item.bucket !== "fail" &&
        item.bucket !== "skipped") ||
      !optionalBlock(item.detail) ||
      !optionalLine(item.url)
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

function parsePullRequestCheckCounts(
  value: unknown,
): WirePullRequestCheckCounts | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WirePullRequestCheckCounts>(value, [
      "passing",
      "pending",
      "failing",
      "skipped",
    ]) ||
    !isNonNegativeInteger(value.passing) ||
    !isNonNegativeInteger(value.pending) ||
    !isNonNegativeInteger(value.failing) ||
    !isNonNegativeInteger(value.skipped)
  ) {
    return null;
  }
  return {
    passing: value.passing,
    pending: value.pending,
    failing: value.failing,
    skipped: value.skipped,
  };
}

export function parseCodeWorkspacePr(
  value: unknown,
): CodeWorkspacePrSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspacePrSnapshot>(value, [
      "remote",
      "dirty",
      "unpushed",
      "ahead",
      "has_upstream",
      "suggested_commit_message",
      "git",
      "pr",
      "gh_found",
      "gh_authenticated",
      "remediation",
      "pushes_as",
      "pushes_as_self",
      "watch",
    ]) ||
    (value.remote !== undefined && typeof value.remote !== "boolean") ||
    typeof value.dirty !== "boolean" ||
    typeof value.unpushed !== "boolean" ||
    !isFiniteNumber(value.ahead) ||
    typeof value.has_upstream !== "boolean" ||
    !blockText(value.suggested_commit_message) ||
    typeof value.gh_found !== "boolean" ||
    (value.gh_authenticated !== undefined &&
      typeof value.gh_authenticated !== "boolean") ||
    !blockText(value.remediation) ||
    !optionalLine(value.pushes_as) ||
    (value.pushes_as_self !== undefined &&
      typeof value.pushes_as_self !== "boolean")
  ) {
    return null;
  }
  const parsed: CodeWorkspacePrSnapshot = {
    ...(value.remote !== undefined ? { remote: value.remote } : {}),
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
    ...(value.pushes_as !== undefined ? { pushes_as: value.pushes_as } : {}),
    ...(value.pushes_as_self !== undefined
      ? { pushes_as_self: value.pushes_as_self }
      : {}),
  };
  if (value.pr !== undefined) {
    const pr = parsePullRequestDigest(value.pr);
    if (!pr) return null;
    parsed.pr = pr;
  }
  if (value.git !== undefined) {
    const git = parseWorkspaceGitState(value.git);
    if (!git) return null;
    parsed.git = git;
  }
  if (value.watch !== undefined) {
    const watch = parseCodeWatch(value.watch);
    if (!watch) return null;
    parsed.watch = watch;
  }
  return parsed;
}

function parseWorkspaceGitState(
  value: unknown,
): NonNullable<CodeWorkspacePrSnapshot["git"]> | null {
  if (!isRecord(value)) return null;
  const counts = [
    "ahead_of_upstream",
    "behind_upstream",
    "changed_files",
    "staged_files",
    "unstaged_files",
    "untracked_files",
    "conflicted_files",
  ] as const;
  if (
    !onlyKeys<NonNullable<CodeWorkspacePrSnapshot["git"]>>(value, [
      "branch",
      "head_sha",
      "base_ref",
      "upstream",
      "branch_has_changes",
      ...counts,
    ]) ||
    !optionalLine(value.branch) ||
    !optionalLine(value.upstream) ||
    !lineText(value.head_sha) ||
    !lineText(value.base_ref) ||
    typeof value.branch_has_changes !== "boolean" ||
    counts.some(
      (key) => !Number.isSafeInteger(value[key]) || (value[key] as number) < 0,
    )
  )
    return null;
  return value as NonNullable<CodeWorkspacePrSnapshot["git"]>;
}

export function parseCodeWatch(value: unknown): CodeWatchSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWatchSnapshot>(value, [
      "id",
      "workspace_id",
      "session_id",
      "pr_number",
      "state",
      "detail",
      "cycles",
      "created_at",
      "updated_at",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.workspace_id) ||
    !wireId(value.session_id) ||
    !isFiniteNumber(value.pr_number) ||
    !isMember(value.state, WATCH_STATES) ||
    !optionalBlock(value.detail) ||
    !isFiniteNumber(value.cycles) ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    workspace_id: value.workspace_id,
    session_id: value.session_id,
    pr_number: value.pr_number,
    state: value.state,
    cycles: value.cycles,
    created_at: value.created_at,
    updated_at: value.updated_at,
    ...(value.detail !== undefined ? { detail: value.detail } : {}),
  };
}

export const TRIGGER_CONDITIONS = new Set<CodeTriggerCondition>([
  "checks_failed",
  "conflicts",
  "changes_requested",
  "review_required",
  "behind",
  "ready_to_merge",
  "merged",
  "closed",
]);

const TRIGGER_ACTIONS = new Set<CodeTriggerAction>(["deliver", "notify"]);

export function parseCodeTrigger(value: unknown): CodeTriggerSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeTriggerSnapshot>(value, [
      "id",
      "repo_id",
      "condition",
      "action",
      "enabled",
      "created_at",
      "updated_at",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.repo_id) ||
    !isMember(value.condition, TRIGGER_CONDITIONS) ||
    !isMember(value.action, TRIGGER_ACTIONS) ||
    typeof value.enabled !== "boolean" ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    repo_id: value.repo_id,
    condition: value.condition,
    action: value.action,
    enabled: value.enabled,
    created_at: value.created_at,
    updated_at: value.updated_at,
  };
}

/**
 * A list where one bad row fails the whole read: a partially parsed rule set
 * would silently show fewer triggers than are actually armed.
 */
export function parseCodeTriggers(
  value: unknown,
): CodeTriggerSnapshot[] | null {
  if (!Array.isArray(value)) {
    return null;
  }
  const triggers: CodeTriggerSnapshot[] = [];
  for (const item of value) {
    const trigger = parseCodeTrigger(item);
    if (!trigger) {
      return null;
    }
    triggers.push(trigger);
  }
  return triggers;
}

const PR_COMMENT_KINDS = new Set<PullRequestCommentKind>([
  "issue",
  "review",
  "inline",
]);

export function parseCodePrComments(
  value: unknown,
): CodePrCommentsSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodePrCommentsSnapshot>(value, ["number", "comments"]) ||
    !isFiniteNumber(value.number) ||
    !Array.isArray(value.comments)
  ) {
    return null;
  }
  const comments: PullRequestComment[] = [];
  for (const item of value.comments) {
    const comment = parsePullRequestComment(item);
    if (!comment) return null;
    comments.push(comment);
  }
  return { number: value.number, comments };
}

export function parsePullRequestComment(
  value: unknown,
): PullRequestComment | null {
  const optionalStringField = (field: unknown) =>
    field === undefined || field === null || lineText(field);
  if (
    !isRecord(value) ||
    !onlyKeys<WirePullRequestComment>(value, [
      "kind",
      "id",
      "author",
      "avatar_url",
      "url",
      "created_at",
      "body",
      "review_state",
      "path",
      "line",
    ]) ||
    !isMember(value.kind, PR_COMMENT_KINDS) ||
    !blockText(value.body) ||
    !optionalStringField(value.id) ||
    !optionalStringField(value.author) ||
    !optionalStringField(value.avatar_url) ||
    !optionalStringField(value.url) ||
    !optionalStringField(value.created_at) ||
    !optionalStringField(value.review_state) ||
    !optionalStringField(value.path) ||
    (value.line !== undefined &&
      value.line !== null &&
      !isFiniteNumber(value.line))
  ) {
    return null;
  }
  return {
    kind: value.kind,
    body: value.body,
    ...(value.id ? { id: value.id } : {}),
    ...(value.author ? { author: value.author } : {}),
    ...(value.avatar_url ? { avatar_url: value.avatar_url } : {}),
    ...(value.url ? { url: value.url } : {}),
    ...(value.created_at ? { created_at: value.created_at } : {}),
    ...(value.review_state ? { review_state: value.review_state } : {}),
    ...(value.path ? { path: value.path } : {}),
    ...(isFiniteNumber(value.line) ? { line: value.line } : {}),
  };
}

export function parseCodeCommit(value: unknown): CodeCommitSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeCommitSnapshot>(value, ["sha", "message", "stat"]) ||
    !wireId(value.sha) ||
    !blockText(value.message)
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
    !nonEmptyLine(value.branch) ||
    !nonEmptyLine(value.remote)
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
    !nonEmptyLine(value.name) ||
    typeof value.success !== "boolean" ||
    (value.exit_code !== undefined && !isFiniteNumber(value.exit_code)) ||
    !rawText(value.stdout) ||
    !rawText(value.stderr) ||
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
