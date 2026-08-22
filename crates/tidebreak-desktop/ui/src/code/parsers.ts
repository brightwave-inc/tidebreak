import type {
  Attention,
  AttentionSource,
  AttentionState,
  CapLevel,
  CodeApprovalSnapshot,
  CodeApprovalState,
  CodeEvent,
  PermissionMode,
  CodeRepoSnapshot,
  CodeSessionActivity,
  CodeSessionLifecycle,
  CodeSessionKind,
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
  CodeWatchState,
  CodeSessionSnapshot,
  CodeFileChange,
  CodeTurnSnapshot,
  CodeTurnStatus,
  CodeUsage,
  CodeTerminalRead,
  CodeTerminalSnapshot,
  CodeWorkspaceDiff,
  CodeWorkspaceFiles,
  CodeWorkspaceSearch,
  CodeWorkspaceBlob,
  CodeWorkspaceTree,
  CodeWorkspacePrSnapshot,
  CodeWatchSnapshot,
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
  ReasoningEffort,
  SequencedCodeEventFrame,
  ToolDetail,
  ToolOutcome,
  CodeSessionDigest,
  CodeSubagentStatus,
  CodeSubagentSummary,
  CodeUpdateNotice,
  CodeCloneDefaults,
  CodeCloneJobSnapshot,
  CodeHarnessInstallSnapshot,
  CodeWorktreeRoot,
  CodeSubscriptionUsage,
  CodeDeliveryActionResult,
  CodeDeliveryCheck,
  CodeDeliveryDeploymentStatus,
  CodeDeliveryPrAttentionReason,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestsPage,
  CodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunDetail,
  CodeDeliveryRunAttentionReason,
  CodeDeliveryRunKind,
  CodeDeliveryRunSummary,
  CodeDeliveryRunsPage,
  CodeDeliverySourceError,
  CodeDeliveryWorkflowJob,
  CodeDeliveryWorkspaceLink,
  CodeGitHubCapability,
  CodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget,
  PullRequestDigest,
  PullRequestComment,
  PullRequestCommentKind,
  CodePrCommentsSnapshot,
  QueuedCodeTurn,
  CodeForkTranscript,
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
  CodeWorkspaceSearch as WireCodeWorkspaceSearch,
  CodeWorkspaceSearchMatch as WireCodeWorkspaceSearchMatch,
  CodeWorkspaceBlob as WireCodeWorkspaceBlob,
  CodeWorkspaceTree as WireCodeWorkspaceTree,
  ImageRef as WireCodeTurnAttachment,
  CodeWorkspaceSnapshot as WireCodeWorkspaceSnapshot,
  CodeWorkspacePrSnapshot as WireCodeWorkspacePrSnapshot,
  CodeTriggerSnapshot as WireCodeTriggerSnapshot,
  CodeWatchSnapshot as WireCodeWatchSnapshot,
  CodeActionSnapshot as WireCodeActionSnapshot,
  CodeCommitSnapshot as WireCodeCommitSnapshot,
  CodePushSnapshot as WireCodePushSnapshot,
  CodeFileChange as WireCodeFileChange,
  Diffstat as WireDiffstat,
  PullRequestDigest as WirePullRequestDigest,
  PullRequestComment as WirePullRequestComment,
  CodePrCommentsSnapshot as WireCodePrCommentsSnapshot,
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
  CodeHarnessInstallSnapshot as WireCodeHarnessInstallSnapshot,
  CodeWorktreeRoot as WireCodeWorktreeRoot,
  CodeDeliveryActionResult as WireCodeDeliveryActionResult,
  CodeDeliveryCheck as WireCodeDeliveryCheck,
  CodeDeliveryDeploymentStatus as WireCodeDeliveryDeploymentStatus,
  CodeDeliveryPullRequestDetail as WireCodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile as WireCodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary as WireCodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestsPage as WireCodeDeliveryPullRequestsPage,
  CodeDeliveryRepositoriesSnapshot as WireCodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunDetail as WireCodeDeliveryRunDetail,
  CodeDeliveryRunSummary as WireCodeDeliveryRunSummary,
  CodeDeliveryRunsPage as WireCodeDeliveryRunsPage,
  CodeDeliverySourceError as WireCodeDeliverySourceError,
  CodeDeliveryWorkflowJob as WireCodeDeliveryWorkflowJob,
  CodeDeliveryWorkspaceLink as WireCodeDeliveryWorkspaceLink,
  CodeForkTranscript as WireCodeForkTranscript,
  CodeGitHubCapability as WireCodeGitHubCapability,
  CodeGitHubRepositoryRef as WireCodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget as WireCodeGitHubRepositoryTarget,
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
const PERMISSION_MODES = new Set<PermissionMode>([
  "plan",
  "ask",
  "auto",
  "allow",
]);
const REASONING_EFFORTS = new Set<ReasoningEffort>([
  "none",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
]);
const SESSION_LIFECYCLES = new Set<CodeSessionLifecycle>([
  "created",
  "idle",
  "running",
  "fenced",
  "ended",
]);
const SESSION_KINDS = new Set<CodeSessionKind>(["interactive", "watch"]);
const SESSION_ACTIVITIES = new Set<CodeSessionActivity>([
  "agent",
  "shell",
  "monitor",
  "subagents",
  "file",
  "search",
  "tool",
]);
const WATCH_STATES = new Set<CodeWatchState>([
  "watching",
  "fixing",
  "blocked",
  "done",
  "stopped",
  "failed",
]);
const SUBAGENT_STATUSES = new Set<CodeSubagentStatus>([
  "running",
  "done",
  "failed",
]);
const WORKSPACE_STATUSES = new Set<CodeWorkspaceStatus>([
  "creating",
  "setup_failed",
  "active",
  "archived",
  "released",
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
  "abandoned",
]);
const TOOL_OUTCOMES = new Set<ToolOutcome>(["succeeded", "failed", "denied"]);
const ATTENTION_SOURCES = new Set<AttentionSource>([
  "structured",
  "heuristic",
  "lifecycle",
  "user",
]);
const USAGE_SOURCES = new Set<CodeSubscriptionUsage["source"]>([
  "model_gateway",
  "direct",
  "unavailable",
]);
const DELIVERY_CHECK_BUCKETS = new Set<CodeDeliveryCheck["bucket"]>([
  "pass",
  "pending",
  "fail",
  "skipped",
]);
const DELIVERY_PR_ATTENTION_REASONS = new Set<CodeDeliveryPrAttentionReason>([
  "changes_requested",
  "checks_failed",
  "conflicts",
  "behind",
  "blocked",
]);
const DELIVERY_RUN_KINDS = new Set<CodeDeliveryRunKind>([
  "workflow_run",
  "deployment",
]);
const DELIVERY_RUN_ATTENTION_REASONS = new Set<CodeDeliveryRunAttentionReason>([
  "failure",
  "timed_out",
  "action_required",
  "startup_failure",
]);

function parseCodeGitHubRepositoryTarget(
  value: unknown,
): CodeGitHubRepositoryTarget | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeGitHubRepositoryTarget>(value, [
      "host",
      "owner",
      "name",
    ]) ||
    !nonEmpty(value.host) ||
    !nonEmpty(value.owner) ||
    !nonEmpty(value.name)
  ) {
    return null;
  }
  return { host: value.host, owner: value.owner, name: value.name };
}

function parseCodeGitHubRepositoryRef(
  value: unknown,
): CodeGitHubRepositoryRef | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeGitHubRepositoryRef>(value, [
      "host",
      "owner",
      "name",
      "name_with_owner",
      "url",
      "default_branch",
      "tidebreak_repo_id",
    ]) ||
    !nonEmpty(value.host) ||
    !nonEmpty(value.owner) ||
    !nonEmpty(value.name) ||
    !nonEmpty(value.name_with_owner) ||
    !nonEmpty(value.url) ||
    !optionalString(value.default_branch) ||
    !optionalString(value.tidebreak_repo_id)
  ) {
    return null;
  }
  return {
    host: value.host,
    owner: value.owner,
    name: value.name,
    name_with_owner: value.name_with_owner,
    url: value.url,
    ...(value.default_branch !== undefined
      ? { default_branch: value.default_branch }
      : {}),
    ...(value.tidebreak_repo_id !== undefined
      ? { tidebreak_repo_id: value.tidebreak_repo_id }
      : {}),
  };
}

function parseCodeGitHubCapability(
  value: unknown,
): CodeGitHubCapability | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeGitHubCapability>(value, [
      "found",
      "authenticated",
      "viewer_login",
      "remediation",
    ]) ||
    typeof value.found !== "boolean" ||
    (value.authenticated !== undefined &&
      typeof value.authenticated !== "boolean") ||
    !optionalString(value.viewer_login) ||
    typeof value.remediation !== "string"
  ) {
    return null;
  }
  return {
    found: value.found,
    remediation: value.remediation,
    ...(value.authenticated !== undefined
      ? { authenticated: value.authenticated }
      : {}),
    ...(value.viewer_login !== undefined
      ? { viewer_login: value.viewer_login }
      : {}),
  };
}

function parseCodeDeliverySourceError(
  value: unknown,
): CodeDeliverySourceError | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliverySourceError>(value, [
      "repository",
      "kind",
      "message",
      "retry_at",
    ]) ||
    !nonEmpty(value.kind) ||
    typeof value.message !== "string" ||
    !optionalString(value.retry_at)
  ) {
    return null;
  }
  const repository =
    value.repository === undefined
      ? undefined
      : parseCodeGitHubRepositoryTarget(value.repository);
  if (value.repository !== undefined && !repository) return null;
  return {
    kind: value.kind,
    message: value.message,
    ...(repository ? { repository } : {}),
    ...(value.retry_at !== undefined ? { retry_at: value.retry_at } : {}),
  };
}

function parseCodeDeliveryWorkspaceLink(
  value: unknown,
): CodeDeliveryWorkspaceLink | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryWorkspaceLink>(value, [
      "workspace_id",
      "repo_id",
      "title",
      "branch_name",
      "status",
      "exact",
    ]) ||
    !nonEmpty(value.workspace_id) ||
    !nonEmpty(value.repo_id) ||
    !nonEmpty(value.title) ||
    !nonEmpty(value.branch_name) ||
    !isMember(value.status, WORKSPACE_STATUSES) ||
    typeof value.exact !== "boolean"
  ) {
    return null;
  }
  return {
    workspace_id: value.workspace_id,
    repo_id: value.repo_id,
    title: value.title,
    branch_name: value.branch_name,
    status: value.status,
    exact: value.exact,
  };
}

function parseCodeDeliveryCheck(value: unknown): CodeDeliveryCheck | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryCheck>(value, [
      "name",
      "bucket",
      "detail",
      "url",
      "workflow_run_id",
    ]) ||
    !nonEmpty(value.name) ||
    !isMember(value.bucket, DELIVERY_CHECK_BUCKETS) ||
    !optionalString(value.detail) ||
    !optionalString(value.url) ||
    (value.workflow_run_id !== undefined &&
      !isPositiveInteger(value.workflow_run_id))
  ) {
    return null;
  }
  return {
    name: value.name,
    bucket: value.bucket,
    ...(value.detail !== undefined ? { detail: value.detail } : {}),
    ...(value.url !== undefined ? { url: value.url } : {}),
    ...(value.workflow_run_id !== undefined
      ? { workflow_run_id: value.workflow_run_id }
      : {}),
  };
}

function parseCodeDeliveryPullRequestSummary(
  value: unknown,
): CodeDeliveryPullRequestSummary | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryPullRequestSummary>(value, [
      "id",
      "repository",
      "number",
      "url",
      "title",
      "state",
      "draft",
      "author",
      "author_avatar_url",
      "head_branch",
      "base_branch",
      "head_sha",
      "review_decision",
      "mergeable",
      "merge_state_status",
      "auto_merge_enabled",
      "checks",
      "attention_reasons",
      "ready_to_merge",
      "workspace_links",
      "labels",
      "created_at",
      "updated_at",
      "merged_at",
      "closed_at",
    ]) ||
    !nonEmpty(value.id) ||
    !isPositiveInteger(value.number) ||
    !nonEmpty(value.url) ||
    !nonEmpty(value.title) ||
    !nonEmpty(value.state) ||
    typeof value.draft !== "boolean" ||
    !optionalString(value.author) ||
    !optionalString(value.author_avatar_url) ||
    !nonEmpty(value.head_branch) ||
    !nonEmpty(value.base_branch) ||
    !optionalString(value.head_sha) ||
    !optionalString(value.review_decision) ||
    !optionalString(value.mergeable) ||
    !optionalString(value.merge_state_status) ||
    typeof value.auto_merge_enabled !== "boolean" ||
    !Array.isArray(value.checks) ||
    !Array.isArray(value.attention_reasons) ||
    !value.attention_reasons.every((reason) =>
      isMember(reason, DELIVERY_PR_ATTENTION_REASONS),
    ) ||
    typeof value.ready_to_merge !== "boolean" ||
    !Array.isArray(value.workspace_links) ||
    !isStringList(value.labels) ||
    !nonEmpty(value.created_at) ||
    !nonEmpty(value.updated_at) ||
    !optionalString(value.merged_at) ||
    !optionalString(value.closed_at)
  ) {
    return null;
  }
  const repository = parseCodeGitHubRepositoryRef(value.repository);
  if (!repository) return null;
  const checks: CodeDeliveryCheck[] = [];
  for (const item of value.checks) {
    const check = parseCodeDeliveryCheck(item);
    if (!check) return null;
    checks.push(check);
  }
  const workspace_links: CodeDeliveryWorkspaceLink[] = [];
  for (const item of value.workspace_links) {
    const link = parseCodeDeliveryWorkspaceLink(item);
    if (!link) return null;
    workspace_links.push(link);
  }
  return {
    id: value.id,
    repository,
    number: value.number,
    url: value.url,
    title: value.title,
    state: value.state,
    draft: value.draft,
    head_branch: value.head_branch,
    base_branch: value.base_branch,
    auto_merge_enabled: value.auto_merge_enabled,
    checks,
    attention_reasons: [...value.attention_reasons],
    ready_to_merge: value.ready_to_merge,
    workspace_links,
    labels: [...value.labels],
    created_at: value.created_at,
    updated_at: value.updated_at,
    ...(value.merged_at !== undefined ? { merged_at: value.merged_at } : {}),
    ...(value.closed_at !== undefined ? { closed_at: value.closed_at } : {}),
    ...(value.author !== undefined ? { author: value.author } : {}),
    ...(value.author_avatar_url !== undefined
      ? { author_avatar_url: value.author_avatar_url }
      : {}),
    ...(value.head_sha !== undefined ? { head_sha: value.head_sha } : {}),
    ...(value.review_decision !== undefined
      ? { review_decision: value.review_decision }
      : {}),
    ...(value.mergeable !== undefined ? { mergeable: value.mergeable } : {}),
    ...(value.merge_state_status !== undefined
      ? { merge_state_status: value.merge_state_status }
      : {}),
  };
}

function parseDeliveryErrors(value: unknown): CodeDeliverySourceError[] | null {
  if (!Array.isArray(value)) return null;
  const errors: CodeDeliverySourceError[] = [];
  for (const item of value) {
    const error = parseCodeDeliverySourceError(item);
    if (!error) return null;
    errors.push(error);
  }
  return errors;
}

export function parseCodeDeliveryRepositories(
  value: unknown,
): CodeDeliveryRepositoriesSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryRepositoriesSnapshot>(value, [
      "capability",
      "repositories",
      "errors",
      "fetched_at",
    ]) ||
    !Array.isArray(value.repositories) ||
    !nonEmpty(value.fetched_at)
  ) {
    return null;
  }
  const capability = parseCodeGitHubCapability(value.capability);
  const errors = parseDeliveryErrors(value.errors);
  if (!capability || !errors) return null;
  const repositories: CodeGitHubRepositoryRef[] = [];
  for (const item of value.repositories) {
    const repository = parseCodeGitHubRepositoryRef(item);
    if (!repository) return null;
    repositories.push(repository);
  }
  return { capability, repositories, errors, fetched_at: value.fetched_at };
}

export function parseCodeDeliveryPullRequestsPage(
  value: unknown,
): CodeDeliveryPullRequestsPage | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryPullRequestsPage>(value, [
      "capability",
      "items",
      "next_cursor",
      "errors",
      "fetched_at",
    ]) ||
    !Array.isArray(value.items) ||
    !optionalString(value.next_cursor) ||
    !nonEmpty(value.fetched_at)
  ) {
    return null;
  }
  const capability = parseCodeGitHubCapability(value.capability);
  const errors = parseDeliveryErrors(value.errors);
  if (!capability || !errors) return null;
  const items: CodeDeliveryPullRequestSummary[] = [];
  for (const item of value.items) {
    const summary = parseCodeDeliveryPullRequestSummary(item);
    if (!summary) return null;
    items.push(summary);
  }
  return {
    capability,
    items,
    errors,
    fetched_at: value.fetched_at,
    ...(value.next_cursor !== undefined
      ? { next_cursor: value.next_cursor }
      : {}),
  };
}

export function parseCodeDeliveryPullRequestDetail(
  value: unknown,
): CodeDeliveryPullRequestDetail | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryPullRequestDetail>(value, [
      "summary",
      "body",
      "labels",
      "assignees",
      "requested_reviewers",
      "changed_files",
      "additions",
      "deletions",
      "commits",
      "merged_by",
      "files",
      "files_truncated",
      "comments",
      "can_mark_ready",
      "can_merge",
      "can_rerun_failed",
      "can_close",
      "can_reopen",
      "can_comment",
    ]) ||
    typeof value.body !== "string" ||
    !isStringList(value.labels) ||
    !isStringList(value.assignees) ||
    !isStringList(value.requested_reviewers) ||
    !isNonNegativeInteger(value.changed_files) ||
    !isNonNegativeInteger(value.additions) ||
    !isNonNegativeInteger(value.deletions) ||
    !isNonNegativeInteger(value.commits) ||
    !optionalString(value.merged_by) ||
    !Array.isArray(value.files) ||
    typeof value.files_truncated !== "boolean" ||
    !Array.isArray(value.comments) ||
    typeof value.can_mark_ready !== "boolean" ||
    typeof value.can_merge !== "boolean" ||
    typeof value.can_rerun_failed !== "boolean" ||
    typeof value.can_close !== "boolean" ||
    typeof value.can_reopen !== "boolean" ||
    typeof value.can_comment !== "boolean"
  ) {
    return null;
  }
  const summary = parseCodeDeliveryPullRequestSummary(value.summary);
  if (!summary) return null;
  const comments: PullRequestComment[] = [];
  for (const item of value.comments) {
    const comment = parsePullRequestComment(item);
    if (!comment) return null;
    comments.push(comment);
  }
  const files: CodeDeliveryPullRequestFile[] = [];
  for (const item of value.files) {
    const file = parseCodeDeliveryPullRequestFile(item);
    if (!file) return null;
    files.push(file);
  }
  return {
    summary,
    body: value.body,
    labels: [...value.labels],
    assignees: [...value.assignees],
    requested_reviewers: [...value.requested_reviewers],
    changed_files: value.changed_files,
    additions: value.additions,
    deletions: value.deletions,
    commits: value.commits,
    files,
    files_truncated: value.files_truncated,
    comments,
    can_mark_ready: value.can_mark_ready,
    can_merge: value.can_merge,
    can_rerun_failed: value.can_rerun_failed,
    can_close: value.can_close,
    can_reopen: value.can_reopen,
    can_comment: value.can_comment,
    ...(value.merged_by !== undefined ? { merged_by: value.merged_by } : {}),
  };
}

function parseCodeDeliveryPullRequestFile(
  value: unknown,
): CodeDeliveryPullRequestFile | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryPullRequestFile>(value, [
      "path",
      "status",
      "additions",
      "deletions",
      "previous_path",
      "patch",
    ]) ||
    !nonEmpty(value.path) ||
    !nonEmpty(value.status) ||
    !isNonNegativeInteger(value.additions) ||
    !isNonNegativeInteger(value.deletions) ||
    !optionalString(value.previous_path) ||
    !optionalString(value.patch)
  ) {
    return null;
  }
  return {
    path: value.path,
    status: value.status,
    additions: value.additions,
    deletions: value.deletions,
    ...(value.previous_path !== undefined
      ? { previous_path: value.previous_path }
      : {}),
    ...(value.patch !== undefined ? { patch: value.patch } : {}),
  };
}

/** Every entry is a string. Used for the PR label and login lists. */
function isStringList(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

function parseCodeDeliveryRunSummary(
  value: unknown,
): CodeDeliveryRunSummary | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryRunSummary>(value, [
      "id",
      "repository",
      "kind",
      "github_id",
      "name",
      "url",
      "status",
      "conclusion",
      "workflow",
      "environment",
      "branch",
      "sha",
      "event",
      "actor",
      "attention_reasons",
      "workspace_links",
      "created_at",
      "updated_at",
    ]) ||
    !nonEmpty(value.id) ||
    !isMember(value.kind, DELIVERY_RUN_KINDS) ||
    !isPositiveInteger(value.github_id) ||
    !nonEmpty(value.name) ||
    !nonEmpty(value.url) ||
    !nonEmpty(value.status) ||
    !optionalString(value.conclusion) ||
    !optionalString(value.workflow) ||
    !optionalString(value.environment) ||
    !optionalString(value.branch) ||
    !optionalString(value.sha) ||
    !optionalString(value.event) ||
    !optionalString(value.actor) ||
    !Array.isArray(value.attention_reasons) ||
    !value.attention_reasons.every((reason) =>
      isMember(reason, DELIVERY_RUN_ATTENTION_REASONS),
    ) ||
    !Array.isArray(value.workspace_links) ||
    !nonEmpty(value.created_at) ||
    !nonEmpty(value.updated_at)
  ) {
    return null;
  }
  const repository = parseCodeGitHubRepositoryRef(value.repository);
  if (!repository) return null;
  const workspace_links: CodeDeliveryWorkspaceLink[] = [];
  for (const item of value.workspace_links) {
    const link = parseCodeDeliveryWorkspaceLink(item);
    if (!link) return null;
    workspace_links.push(link);
  }
  return {
    id: value.id,
    repository,
    kind: value.kind,
    github_id: value.github_id,
    name: value.name,
    url: value.url,
    status: value.status,
    attention_reasons: [...value.attention_reasons],
    workspace_links,
    created_at: value.created_at,
    updated_at: value.updated_at,
    ...(value.conclusion !== undefined ? { conclusion: value.conclusion } : {}),
    ...(value.workflow !== undefined ? { workflow: value.workflow } : {}),
    ...(value.environment !== undefined
      ? { environment: value.environment }
      : {}),
    ...(value.branch !== undefined ? { branch: value.branch } : {}),
    ...(value.sha !== undefined ? { sha: value.sha } : {}),
    ...(value.event !== undefined ? { event: value.event } : {}),
    ...(value.actor !== undefined ? { actor: value.actor } : {}),
  };
}

export function parseCodeDeliveryRunsPage(
  value: unknown,
): CodeDeliveryRunsPage | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryRunsPage>(value, [
      "capability",
      "items",
      "next_cursor",
      "errors",
      "fetched_at",
    ]) ||
    !Array.isArray(value.items) ||
    !optionalString(value.next_cursor) ||
    !nonEmpty(value.fetched_at)
  ) {
    return null;
  }
  const capability = parseCodeGitHubCapability(value.capability);
  const errors = parseDeliveryErrors(value.errors);
  if (!capability || !errors) return null;
  const items: CodeDeliveryRunSummary[] = [];
  for (const item of value.items) {
    const summary = parseCodeDeliveryRunSummary(item);
    if (!summary) return null;
    items.push(summary);
  }
  return {
    capability,
    items,
    errors,
    fetched_at: value.fetched_at,
    ...(value.next_cursor !== undefined
      ? { next_cursor: value.next_cursor }
      : {}),
  };
}

function parseCodeDeliveryWorkflowJob(
  value: unknown,
): CodeDeliveryWorkflowJob | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryWorkflowJob>(value, [
      "id",
      "name",
      "status",
      "conclusion",
      "url",
      "started_at",
      "completed_at",
      "failed_steps",
    ]) ||
    !isPositiveInteger(value.id) ||
    !nonEmpty(value.name) ||
    !nonEmpty(value.status) ||
    !optionalString(value.conclusion) ||
    !nonEmpty(value.url) ||
    !nullableString(value.started_at) ||
    !nullableString(value.completed_at) ||
    !Array.isArray(value.failed_steps) ||
    !value.failed_steps.every((item) => typeof item === "string")
  ) {
    return null;
  }
  return {
    id: value.id,
    name: value.name,
    status: value.status,
    url: value.url,
    started_at: value.started_at,
    completed_at: value.completed_at,
    failed_steps: [...value.failed_steps],
    ...(value.conclusion !== undefined ? { conclusion: value.conclusion } : {}),
  };
}

function parseCodeDeliveryDeploymentStatus(
  value: unknown,
): CodeDeliveryDeploymentStatus | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryDeploymentStatus>(value, [
      "id",
      "state",
      "description",
      "environment_url",
      "log_url",
      "created_at",
    ]) ||
    !isPositiveInteger(value.id) ||
    !nonEmpty(value.state) ||
    typeof value.description !== "string" ||
    !optionalString(value.environment_url) ||
    !optionalString(value.log_url) ||
    !nonEmpty(value.created_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    state: value.state,
    description: value.description,
    created_at: value.created_at,
    ...(value.environment_url !== undefined
      ? { environment_url: value.environment_url }
      : {}),
    ...(value.log_url !== undefined ? { log_url: value.log_url } : {}),
  };
}

export function parseCodeDeliveryRunDetail(
  value: unknown,
): CodeDeliveryRunDetail | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryRunDetail>(value, [
      "summary",
      "jobs",
      "deployment_statuses",
      "can_rerun_failed",
    ]) ||
    !Array.isArray(value.jobs) ||
    !Array.isArray(value.deployment_statuses) ||
    typeof value.can_rerun_failed !== "boolean"
  ) {
    return null;
  }
  const summary = parseCodeDeliveryRunSummary(value.summary);
  if (!summary) return null;
  const jobs: CodeDeliveryWorkflowJob[] = [];
  for (const item of value.jobs) {
    const job = parseCodeDeliveryWorkflowJob(item);
    if (!job) return null;
    jobs.push(job);
  }
  const deployment_statuses: CodeDeliveryDeploymentStatus[] = [];
  for (const item of value.deployment_statuses) {
    const status = parseCodeDeliveryDeploymentStatus(item);
    if (!status) return null;
    deployment_statuses.push(status);
  }
  return {
    summary,
    jobs,
    deployment_statuses,
    can_rerun_failed: value.can_rerun_failed,
  };
}

export function parseCodeDeliveryActionResult(
  value: unknown,
): CodeDeliveryActionResult | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryActionResult>(value, ["success", "message"]) ||
    typeof value.success !== "boolean" ||
    typeof value.message !== "string"
  ) {
    return null;
  }
  return { success: value.success, message: value.message };
}

export function parseCodeSubscriptionUsage(
  value: unknown,
): CodeSubscriptionUsage | null {
  if (
    !isRecord(value) ||
    !isMember(value.source, USAGE_SOURCES) ||
    !Array.isArray(value.providers) ||
    !Array.isArray(value.diagnostics) ||
    !value.diagnostics.every((item) => typeof item === "string")
  ) {
    return null;
  }
  const providers: CodeSubscriptionUsage["providers"] = [];
  for (const provider of value.providers) {
    if (
      !isRecord(provider) ||
      !nonEmpty(provider.id) ||
      !nonEmpty(provider.label) ||
      !Array.isArray(provider.accounts)
    ) {
      return null;
    }
    const accounts = [];
    for (const account of provider.accounts) {
      if (
        !isRecord(account) ||
        !nonEmpty(account.id) ||
        !nonEmpty(account.label) ||
        typeof account.is_own !== "boolean" ||
        typeof account.state !== "string" ||
        (account.updated_at_unix_seconds !== undefined &&
          !isFiniteNumber(account.updated_at_unix_seconds)) ||
        !Array.isArray(account.windows)
      ) {
        return null;
      }
      const windows = [];
      for (const window of account.windows) {
        if (
          !isRecord(window) ||
          !nonEmpty(window.key) ||
          !nonEmpty(window.label) ||
          !isFiniteNumber(window.used_percent) ||
          (window.resets_at_unix_seconds !== undefined &&
            !isFiniteNumber(window.resets_at_unix_seconds)) ||
          !optionalString(window.status) ||
          !optionalString(window.model_scope)
        ) {
          return null;
        }
        windows.push({
          key: window.key,
          label: window.label,
          used_percent: window.used_percent,
          ...(window.resets_at_unix_seconds !== undefined
            ? { resets_at_unix_seconds: window.resets_at_unix_seconds }
            : {}),
          ...(window.status !== undefined ? { status: window.status } : {}),
          ...(window.model_scope !== undefined
            ? { model_scope: window.model_scope }
            : {}),
        });
      }
      accounts.push({
        id: account.id,
        label: account.label,
        is_own: account.is_own,
        state: account.state,
        windows,
        ...(account.updated_at_unix_seconds !== undefined
          ? { updated_at_unix_seconds: account.updated_at_unix_seconds }
          : {}),
      });
    }
    providers.push({ id: provider.id, label: provider.label, accounts });
  }
  return {
    source: value.source,
    providers,
    diagnostics: [...value.diagnostics],
  };
}

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

export function parseCodeHarnessInstall(
  value: unknown,
): CodeHarnessInstallSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeHarnessInstallSnapshot>(value, [
      "kind",
      "version",
      "phase",
      "done",
      "error",
    ]) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    typeof value.phase !== "string" ||
    typeof value.done !== "boolean" ||
    !optionalString(value.version) ||
    !optionalString(value.error)
  ) {
    return null;
  }
  return {
    kind: value.kind,
    phase: value.phase,
    done: value.done,
    ...(value.version !== undefined ? { version: value.version } : {}),
    ...(value.error !== undefined ? { error: value.error } : {}),
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

export function parseCodeForkTranscript(
  value: unknown,
): CodeForkTranscript | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeForkTranscript>(value, [
      "path",
      "byte_len",
      "turns",
      "truncated",
    ]) ||
    typeof value.path !== "string" ||
    typeof value.byte_len !== "number" ||
    typeof value.turns !== "number" ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  return {
    path: value.path,
    byte_len: value.byte_len,
    turns: value.turns,
    truncated: value.truncated,
  };
}

export function parseCodeWorktreeRoot(value: unknown): CodeWorktreeRoot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorktreeRoot>(value, [
      "root",
      "effective_root",
      "default_root",
    ]) ||
    !optionalString(value.root) ||
    typeof value.effective_root !== "string" ||
    typeof value.default_root !== "string"
  ) {
    return null;
  }
  return {
    effective_root: value.effective_root,
    default_root: value.default_root,
    ...(value.root !== undefined ? { root: value.root } : {}),
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
    ...(value.archived_at !== undefined
      ? { archived_at: value.archived_at }
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
    field === undefined || field === null || typeof field === "string";
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
    !nonEmpty(value.state) ||
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
  return {
    number: value.number,
    state: value.state,
    ...(value.url ? { url: value.url } : {}),
    ...(value.title ? { title: value.title } : {}),
    ...(value.checks_summary ? { checks_summary: value.checks_summary } : {}),
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
        item.bucket !== "fail" &&
        item.bucket !== "skipped") ||
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
      "watch",
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
  if (value.watch !== undefined) {
    const watch = parseCodeWatch(value.watch);
    if (!watch) return null;
    parsed.watch = watch;
  }
  return parsed;
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
    !nonEmpty(value.id) ||
    !nonEmpty(value.workspace_id) ||
    !nonEmpty(value.session_id) ||
    !isFiniteNumber(value.pr_number) ||
    !isMember(value.state, WATCH_STATES) ||
    (value.detail !== undefined && typeof value.detail !== "string") ||
    !isFiniteNumber(value.cycles) ||
    !nonEmpty(value.created_at) ||
    !nonEmpty(value.updated_at)
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

const TRIGGER_CONDITIONS = new Set<CodeTriggerCondition>([
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
    !nonEmpty(value.id) ||
    !nonEmpty(value.repo_id) ||
    !isMember(value.condition, TRIGGER_CONDITIONS) ||
    !isMember(value.action, TRIGGER_ACTIONS) ||
    typeof value.enabled !== "boolean" ||
    !nonEmpty(value.created_at) ||
    !nonEmpty(value.updated_at)
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

function parsePullRequestComment(value: unknown): PullRequestComment | null {
  const optionalStringField = (field: unknown) =>
    field === undefined || field === null || typeof field === "string";
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
    typeof value.body !== "string" ||
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
      "created_at",
    ]) ||
    !nonEmpty(value.id) ||
    !nonEmpty(value.workspace_id) ||
    !isMember(value.kind, SESSION_KINDS) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    !optionalString(value.harness_version) ||
    !optionalString(value.harness_resume_ref) ||
    !optionalString(value.model) ||
    (value.reasoning_effort !== undefined &&
      !isMember(value.reasoning_effort, REASONING_EFFORTS)) ||
    // Serialized unconditionally, but tolerate its absence: a session row
    // written before fast mode existed reads as off, which is what it was.
    (value.fast_mode !== undefined && typeof value.fast_mode !== "boolean") ||
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
    kind: value.kind,
    harness_kind: value.harness_kind,
    permission_mode: value.permission_mode,
    lifecycle: value.lifecycle,
    attention,
    unrecognized_event_count: value.unrecognized_event_count,
    created_at: value.created_at,
    fast_mode: value.fast_mode === true,
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
 */
export function liveCodeSessions(
  sessions: readonly CodeSessionSnapshot[],
): CodeSessionSnapshot[] {
  return sessions
    .filter(
      (session) =>
        session.kind === "interactive" && session.lifecycle !== "ended",
    )
    .sort(
      (left, right) =>
        left.created_at.localeCompare(right.created_at) ||
        left.id.localeCompare(right.id),
    );
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
      "attachments",
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
    user_input: value.user_input,
    started_at: value.started_at,
    attachments,
    ...(usage ? { usage } : {}),
    ...(value.checkpoint_ref !== undefined
      ? { checkpoint_ref: value.checkpoint_ref }
      : {}),
    ...(diffstat ? { diffstat } : {}),
    ...(value.ended_at !== undefined ? { ended_at: value.ended_at } : {}),
  };
}

const IMAGE_MEDIA_TYPES = new Set<import("../generated/wire").ImageMediaType>([
  "png",
  "jpeg",
  "webp",
  "gif",
]);

function parseCodeTurnAttachments(
  value: unknown,
): import("../generated/wire").ImageRef[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;
  const attachments: import("../generated/wire").ImageRef[] = [];
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
      !nonEmpty(item.blob_id) ||
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
    !onlyKeys<WireQueuedCodeTurn>(value, [
      "session_id",
      "message",
      "position",
    ]) ||
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

export function parseCodeWorkspaceTree(
  value: unknown,
): CodeWorkspaceTree | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceTree>(value, ["paths", "truncated"]) ||
    !Array.isArray(value.paths) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const paths: string[] = [];
  for (const item of value.paths) {
    if (typeof item !== "string" || item.length === 0) return null;
    paths.push(item);
  }
  return { paths, truncated: value.truncated };
}

export function parseCodeWorkspaceSearch(
  value: unknown,
): CodeWorkspaceSearch | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceSearch>(value, ["matches", "truncated"]) ||
    !Array.isArray(value.matches) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  const matches: WireCodeWorkspaceSearchMatch[] = [];
  for (const item of value.matches) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireCodeWorkspaceSearchMatch>(item, [
        "path",
        "line_number",
        "line",
      ]) ||
      !nonEmpty(item.path) ||
      !isFiniteNumber(item.line_number) ||
      item.line_number < 1 ||
      typeof item.line !== "string"
    ) {
      return null;
    }
    matches.push({
      path: item.path,
      line_number: item.line_number,
      line: item.line,
    });
  }
  return { matches, truncated: value.truncated };
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

export function parseCodeWorkspaceBlob(
  value: unknown,
): CodeWorkspaceBlob | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceBlob>(value, [
      "path",
      "content",
      "truncated",
      "binary",
    ]) ||
    typeof value.path !== "string" ||
    typeof value.content !== "string" ||
    typeof value.truncated !== "boolean" ||
    typeof value.binary !== "boolean"
  ) {
    return null;
  }
  return {
    path: value.path,
    content: value.content,
    truncated: value.truncated,
    binary: value.binary,
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

export type ParsedHarnessModel = {
  id: string;
  label: string;
  default: boolean;
  reasoning_efforts: ReasoningEffort[];
  fast_mode: boolean;
};

function parseEfforts(value: unknown): ReasoningEffort[] {
  // A server that predates the field, or a level this build cannot label,
  // narrows the offer rather than failing the whole list.
  return Array.isArray(value)
    ? value.filter((level): level is ReasoningEffort =>
        isMember(level, REASONING_EFFORTS),
      )
    : [];
}

export function parseHarnessModelList(value: unknown): {
  kind: HarnessKind;
  models: ParsedHarnessModel[];
  reasoning_efforts: ReasoningEffort[];
} | null {
  if (
    !isRecord(value) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !Array.isArray(value.models)
  ) {
    return null;
  }
  const models: ParsedHarnessModel[] = [];
  for (const item of value.models) {
    if (
      !isRecord(item) ||
      typeof item.id !== "string" ||
      typeof item.label !== "string" ||
      typeof item.default !== "boolean"
    ) {
      return null;
    }
    models.push({
      id: item.id,
      label: item.label,
      default: item.default,
      reasoning_efforts: parseEfforts(item.reasoning_efforts),
      // A server that predates the field offers no fast mode, which is the
      // same thing a row without the tier says.
      fast_mode: item.fast_mode === true,
    });
  }
  return {
    kind: value.kind,
    models,
    reasoning_efforts: parseEfforts(value.reasoning_efforts),
  };
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
      "commands",
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
  const commands = parseHarnessCommands(value.commands);
  if (!commands) return null;
  return {
    kind: value.kind,
    found: value.found,
    tier: value.tier,
    caps,
    remediation: value.remediation,
    stderr: value.stderr,
    unrecognized_event_count: value.unrecognized_event_count,
    commands,
    ...(value.path !== undefined ? { path: value.path } : {}),
    ...(value.version !== undefined ? { version: value.version } : {}),
    ...(value.authenticated !== undefined
      ? { authenticated: value.authenticated }
      : {}),
  };
}

function parseHarnessCommands(
  value: unknown,
): { name: string; description: string }[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value)) return null;
  const commands: { name: string; description: string }[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      typeof item.name !== "string" ||
      item.name.length === 0 ||
      typeof item.description !== "string"
    ) {
      return null;
    }
    commands.push({ name: item.name, description: item.description });
  }
  return commands;
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
      "image_input",
      "slash_commands",
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
    !isMember(value.native_interrupt, CAP_LEVELS) ||
    !isMember(value.image_input, CAP_LEVELS) ||
    !isMember(value.slash_commands, CAP_LEVELS)
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
    image_input: value.image_input,
    slash_commands: value.slash_commands,
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
    ]) ||
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
        ...(value.resume_ref !== undefined
          ? { resume_ref: value.resume_ref }
          : {}),
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
    case "reasoning_delta":
    case "user_steered":
      if (
        !onlyKeys(value, ["type", "text"]) ||
        typeof value.text !== "string"
      ) {
        return null;
      }
      return { type: value.type, text: value.text } as CodeEvent;
    case "assistant_message":
      // A subagent's message names its spanning `Task` call (ADR 0052).
      if (
        !onlyKeys(value, ["type", "text", "parent_call_id"]) ||
        typeof value.text !== "string" ||
        (value.parent_call_id !== undefined && !nonEmpty(value.parent_call_id))
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
        !nonEmpty(value.call_id) ||
        !nonEmpty(value.name) ||
        (value.parent_call_id !== undefined && !nonEmpty(value.parent_call_id))
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
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "tool_completed" }>>(value, [
          "type",
          "call_id",
          "outcome",
          "preview",
          "detail",
          "parent_call_id",
        ]) ||
        !nonEmpty(value.call_id) ||
        !isMember(value.outcome, TOOL_OUTCOMES) ||
        typeof value.preview !== "string" ||
        (value.parent_call_id !== undefined && !nonEmpty(value.parent_call_id))
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
        (value.decision.type !== "approve" &&
          value.decision.type !== "deny" &&
          value.decision.type !== "abandoned")
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
        !onlyKeys<Extract<WireCodeEvent, { type: "attention_changed" }>>(
          value,
          ["type", "state", "source"],
        ) ||
        !isMember(value.source, ATTENTION_SOURCES)
      ) {
        return null;
      }
      const state = parseAttentionState(value.state);
      if (!state) return null;
      return { type: "attention_changed", state, source: value.source };
    }
    case "file_changed": {
      if (
        !onlyKeys<Extract<WireCodeEvent, { type: "file_changed" }>>(value, [
          "type",
          "path",
          "kind",
          "diffstat",
        ]) ||
        typeof value.path !== "string" ||
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
  // `context_tokens` is serde-defaulted, so a turn journaled before the field
  // existed omits it. Rejecting the whole object over a missing occupancy
  // reading would throw away the spend counts beside it; absent reads as no
  // reading, which is what zero already means here.
  const contextTokens = value.context_tokens;
  if (contextTokens !== undefined && !isFiniteNumber(contextTokens)) {
    return null;
  }
  return {
    input_tokens: value.input_tokens,
    output_tokens: value.output_tokens,
    cache_read_input_tokens: value.cache_read_input_tokens,
    cache_creation_input_tokens: value.cache_creation_input_tokens,
    context_tokens: contextTokens ?? 0,
  };
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
  if (
    value.type === "repeated_turn_failures" &&
    typeof value.count === "number" &&
    typeof value.detail === "string"
  ) {
    return {
      type: "repeated_turn_failures",
      count: value.count,
      detail: value.detail,
    };
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

function nullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
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

function isNonNegativeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

function isPositiveInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) > 0;
}

/** `undefined` stays undefined; a present list must be well-formed. */
function parseSubagents(value: unknown): CodeSubagentSummary[] | null {
  if (!Array.isArray(value)) return null;
  const subagents: CodeSubagentSummary[] = [];
  for (const item of value) {
    if (
      !isRecord(item) ||
      !onlyKeys<CodeSubagentSummary>(item, ["call_id", "name", "status"]) ||
      !nonEmpty(item.call_id) ||
      typeof item.name !== "string" ||
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

export function parseCodeSessionDigest(
  value: unknown,
): CodeSessionDigest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeSessionDigest>(value, [
      "workspace",
      "session",
      "kind",
      "lifecycle",
      "attention",
      "title",
      "turn_count",
      "activity",
      "pr_state",
      "watch_state",
      "watch_detail",
      "watch_cycles",
      "subagents",
    ]) ||
    !nonEmpty(value.workspace) ||
    !nonEmpty(value.session) ||
    !isMember(value.kind, SESSION_KINDS) ||
    !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
    typeof value.title !== "string" ||
    !isFiniteNumber(value.turn_count) ||
    (value.activity !== undefined &&
      !isMember(value.activity, SESSION_ACTIVITIES)) ||
    (value.watch_state !== undefined &&
      !isMember(value.watch_state, WATCH_STATES)) ||
    !optionalString(value.watch_detail) ||
    (value.watch_cycles !== undefined && !isFiniteNumber(value.watch_cycles))
  ) {
    return null;
  }
  const attention = parseAttention(value.attention);
  if (!attention) return null;
  const pr_state =
    value.pr_state === undefined ? undefined : parsePrState(value.pr_state);
  if (value.pr_state !== undefined && !pr_state) return null;
  const subagents =
    value.subagents === undefined ? undefined : parseSubagents(value.subagents);
  if (value.subagents !== undefined && !subagents) return null;
  return {
    workspace: value.workspace,
    session: value.session,
    kind: value.kind,
    lifecycle: value.lifecycle,
    attention,
    title: value.title,
    turn_count: value.turn_count,
    ...(value.activity !== undefined ? { activity: value.activity } : {}),
    ...(pr_state ? { pr_state } : {}),
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
          "lifecycle",
          "attention",
          "title",
          "turn_count",
          "activity",
          "pr_state",
          "watch_state",
          "watch_detail",
          "watch_cycles",
          "subagents",
        ]) ||
        !nonEmpty(value.workspace) ||
        !nonEmpty(value.session) ||
        !isMember(value.kind, SESSION_KINDS) ||
        !isMember(value.lifecycle, SESSION_LIFECYCLES) ||
        typeof value.title !== "string" ||
        !isFiniteNumber(value.turn_count) ||
        (value.activity !== undefined &&
          !isMember(value.activity, SESSION_ACTIVITIES)) ||
        (value.watch_state !== undefined &&
          !isMember(value.watch_state, WATCH_STATES)) ||
        !optionalString(value.watch_detail) ||
        (value.watch_cycles !== undefined &&
          !isFiniteNumber(value.watch_cycles))
      ) {
        return null;
      }
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
      return {
        type: "digest",
        workspace: value.workspace,
        session: value.session,
        kind: value.kind,
        lifecycle: value.lifecycle,
        attention,
        title: value.title,
        turn_count: value.turn_count,
        ...(value.activity !== undefined ? { activity: value.activity } : {}),
        ...(pr_state ? { pr_state } : {}),
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
    case "harness_install": {
      if (
        !onlyKeys<Extract<WireCodeUpdateNotice, { type: "harness_install" }>>(
          value,
          ["type", "kind", "version", "phase", "done", "error"],
        ) ||
        !isMember(value.kind, HARNESS_KINDS) ||
        typeof value.phase !== "string" ||
        typeof value.done !== "boolean" ||
        !optionalString(value.version) ||
        !optionalString(value.error)
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
  return parsePullRequestDigest(value);
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
