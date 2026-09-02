import {
  MAX_WIRE_CURSOR_CHARS,
  MAX_WIRE_ID_CHARS,
  MAX_WIRE_TIMESTAMP_CHARS,
  bounded,
  boundedBlock,
  boundedRaw,
  boundedStringList,
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isPositiveInteger,
  isRecord,
  nonEmptyBounded,
  onlyKeys,
} from "../lib/wireDecode";
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
  CodeGrantSnapshot,
  CodeConnectPage,
  CodeActionSnapshot,
  CodeCommitSnapshot,
  CodePushSnapshot,
  Diffstat,
  FileChangeKind,
  CodeWorkspaceStatus,
  FenceReason,
  HarnessCaps,
  HarnessAuthMode,
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
  CodeRepoSource,
  CodeRepoSources,
  CodeCloneJobSnapshot,
  CodeHarnessInstallSnapshot,
  CodeWorktreeRoot,
  CodeAnalyticsDay,
  CodeAnalyticsHarness,
  CodeAnalyticsModel,
  CodeAnalyticsPricingCoverage,
  CodeAnalyticsRange,
  CodeAnalyticsRepository,
  CodeAnalyticsSnapshot,
  CodeAnalyticsTotals,
  CodeSubscriptionUsage,
  CodeDeliveryActionResult,
  CodeDeliveryCheck,
  CodeDeliveryDeploymentStatus,
  CodeDeliveryPrAttentionReason,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryStackMember,
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
  CodePullRequestRelation,
  CodeWorkspacePullRequestFact,
  CodeWorkspacePullRequests,
  PullRequestDigest,
  PullRequestComment,
  PullRequestCommentKind,
  CodePrCommentsSnapshot,
  QueuedCodeTurn,
  CodeCheckLog,
  CodeCheckLogError,
  CodeCheckLogsSnapshot,
  CodeForkTranscript,
} from "../api/types";
import type {
  CodeEvent as WireCodeEvent,
  CodeRepoSnapshot as WireCodeRepoSnapshot,
  CodeSessionSnapshot as WireCodeSessionSnapshot,
  CodeSessionExternalOrigin,
  CodeTurnSnapshot as WireCodeTurnSnapshot,
  QueuedCodeTurn as WireQueuedCodeTurn,
  CodeTerminalRead as WireCodeTerminalRead,
  CodeTerminalSnapshot as WireCodeTerminalSnapshot,
  CodeWorkspaceDiff as WireCodeWorkspaceDiff,
  CodeWorkspaceFiles as WireCodeWorkspaceFiles,
  CodeWorkspaceHistorySearchMatch as WireCodeWorkspaceHistorySearchMatch,
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
  PullRequestCheckCounts as WirePullRequestCheckCounts,
  PullRequestComment as WirePullRequestComment,
  CodePrCommentsSnapshot as WireCodePrCommentsSnapshot,
  HarnessCaps as WireHarnessCaps,
  HarnessDoctorEntry as WireHarnessDoctorEntry,
  HarnessDoctorReport as WireHarnessDoctorReport,
  HarnessModelSource as WireHarnessModelSource,
  HarnessUpdateChannel as WireHarnessUpdateChannel,
  QuickAction as WireQuickAction,
  SequencedCodeEventFrame as WireSequencedCodeEventFrame,
  ToolDetail as WireToolDetail,
  CodeSessionDigest as WireCodeSessionDigest,
  CodeUpdateNotice as WireCodeUpdateNotice,
  CodeTurnRewriteState,
  CodeCloneDefaults as WireCodeCloneDefaults,
  CodeRepoSource as WireCodeRepoSource,
  CodeRepoSources as WireCodeRepoSources,
  CodeGithubRepositories as WireCodeGithubRepositories,
  CodeGithubRepository as WireCodeGithubRepository,
  CodeCloneJobSnapshot as WireCodeCloneJobSnapshot,
  CodeHarnessInstallSnapshot as WireCodeHarnessInstallSnapshot,
  CodeWorktreeRoot as WireCodeWorktreeRoot,
  CodeAnalyticsDay as WireCodeAnalyticsDay,
  CodeAnalyticsHarness as WireCodeAnalyticsHarness,
  CodeAnalyticsModel as WireCodeAnalyticsModel,
  CodeAnalyticsPricingCoverage as WireCodeAnalyticsPricingCoverage,
  CodeAnalyticsRepository as WireCodeAnalyticsRepository,
  CodeAnalyticsSnapshot as WireCodeAnalyticsSnapshot,
  CodeAnalyticsTotals as WireCodeAnalyticsTotals,
  CodeDeliveryActionResult as WireCodeDeliveryActionResult,
  CodeDeliveryCheck as WireCodeDeliveryCheck,
  CodeDeliveryDeploymentStatus as WireCodeDeliveryDeploymentStatus,
  CodeDeliveryPullRequestDetail as WireCodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestFile as WireCodeDeliveryPullRequestFile,
  CodeDeliveryStackMember as WireCodeDeliveryStackMember,
  CodeDeliveryPullRequestSummary as WireCodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestsPage as WireCodeDeliveryPullRequestsPage,
  CodeDeliveryRepositoriesSnapshot as WireCodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunDetail as WireCodeDeliveryRunDetail,
  CodeDeliveryRunSummary as WireCodeDeliveryRunSummary,
  CodeDeliveryRunsPage as WireCodeDeliveryRunsPage,
  CodeDeliverySourceError as WireCodeDeliverySourceError,
  CodeDeliveryWorkflowJob as WireCodeDeliveryWorkflowJob,
  CodeDeliveryWorkspaceLink as WireCodeDeliveryWorkspaceLink,
  CodeWorkspacePullRequestFact as WireCodeWorkspacePullRequestFact,
  CodeWorkspacePullRequests as WireCodeWorkspacePullRequests,
  CodeCheckLog as WireCodeCheckLog,
  CodeCheckLogError as WireCodeCheckLogError,
  CodeCheckLogsSnapshot as WireCodeCheckLogsSnapshot,
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

// ---------------------------------------------------------------------------
// String bounds
// ---------------------------------------------------------------------------
//
// Every string this decoder keeps goes through one of three tiers, chosen by
// how the field is drawn rather than by who wrote it:
//
// - Line: drawn on one line, so a control or bidirectional character could
//   redraw or reorder it. Titles, names, branch and base refs, paths, URLs,
//   logins, statuses, models, versions, remediation headlines. The limit
//   holds a PATH_MAX-sized path or a long URL; GitHub caps titles at 256 and
//   refs well under that.
// - Block: drawn as a block, so line breaks, carriage returns, and tabs are
//   structure while everything else a line rejects is still rejected. PR and
//   comment bodies (GitHub caps these at 65,536 characters), commit messages,
//   setup and archive scripts, quick-action commands, model text and
//   reasoning, remediation and error messages, recaps, fence details.
// - Raw: verbatim data the reader asked to see as it is, rendered by a pane
//   that escapes nothing and expects terminal escapes and carriage returns:
//   blob content, diffs and patches, terminal reads, command stdout/stderr,
//   tool result previews, search hit lines and the history excerpts cut from
//   prompts and tool output, the harness's raw approval JSON, and the user's
//   own prompt and steer text. Only the length is bounded,
//   with headroom over the server's own truncation points (512 KiB blobs,
//   256 KiB diffs).
//
// Ids, timestamps, and cursors share the chat decoder's named limits so the
// two clients agree on what a valid payload is. Enum-like discriminators
// (`type`, `kind`) stay presence-only: they are matched, never drawn.

/** Longest one-line field: a PATH_MAX path or a long URL still fits. */
const MAX_CODE_LINE_CHARS = 4_096;

/** Longest authored block: sixteen GitHub-sized bodies, or a long model reply. */
const MAX_CODE_BLOCK_CHARS = 1_048_576;

/** Longest verbatim payload: eight times the server's blob cap. */
const MAX_CODE_RAW_CHARS = 4_194_304;

const lineText = (value: unknown): value is string =>
  bounded(value, MAX_CODE_LINE_CHARS);
const nonEmptyLine = (value: unknown): value is string =>
  nonEmptyBounded(value, MAX_CODE_LINE_CHARS);
const optionalLine = (value: unknown): value is string | undefined =>
  value === undefined || bounded(value, MAX_CODE_LINE_CHARS);
const lineList = (value: unknown): value is string[] =>
  boundedStringList(value, MAX_CODE_LINE_CHARS);

const blockText = (value: unknown): value is string =>
  boundedBlock(value, MAX_CODE_BLOCK_CHARS);
const optionalBlock = (value: unknown): value is string | undefined =>
  value === undefined || boundedBlock(value, MAX_CODE_BLOCK_CHARS);

const rawText = (value: unknown): value is string =>
  boundedRaw(value, MAX_CODE_RAW_CHARS);
const optionalRaw = (value: unknown): value is string | undefined =>
  value === undefined || boundedRaw(value, MAX_CODE_RAW_CHARS);

const wireId = (value: unknown): value is string =>
  nonEmptyBounded(value, MAX_WIRE_ID_CHARS);
const optionalWireId = (value: unknown): value is string | undefined =>
  value === undefined || nonEmptyBounded(value, MAX_WIRE_ID_CHARS);
/**
 * An id or `null`. A session that binds no workspace (the in-process
 * engine's, decision 0048 step 5) serializes `workspace_id: null` on its
 * snapshot and `workspace: null` on its digest; the key is always present.
 */
const nullableWireId = (value: unknown): value is string | null =>
  value === null || nonEmptyBounded(value, MAX_WIRE_ID_CHARS);

const timestamp = (value: unknown): value is string =>
  nonEmptyBounded(value, MAX_WIRE_TIMESTAMP_CHARS);
const optionalTimestamp = (value: unknown): value is string | undefined =>
  value === undefined || nonEmptyBounded(value, MAX_WIRE_TIMESTAMP_CHARS);
const nullableTimestamp = (value: unknown): value is string | null =>
  value === null || nonEmptyBounded(value, MAX_WIRE_TIMESTAMP_CHARS);

const optionalCursor = (value: unknown): value is string | undefined =>
  value === undefined || nonEmptyBounded(value, MAX_WIRE_CURSOR_CHARS);

// Every kind the server can name on a session, not just the ones the create
// picker offers: the in-process engine reports `internal` (decision 0048
// step 5), and a session it runs must parse like any other.
const HARNESS_KINDS = new Set<HarnessKind>([
  "claude_code",
  "codex",
  "opencode",
  "grok",
  "internal",
]);
const HARNESS_TIERS = new Set<HarnessTier>([
  "reference",
  "secondary",
  "tertiary",
  "best_effort",
]);
const HARNESS_AUTH_MODES = new Set<HarnessAuthMode>([
  "local_sign_in",
  "gateway_managed",
  "gateway_relay",
  "hosted_unavailable",
]);
const HARNESS_UPDATE_CHANNELS = new Set<WireHarnessUpdateChannel>([
  "pinned",
  "latest",
]);
const HARNESS_MODEL_SOURCES = new Set<WireHarnessModelSource>([
  "harness",
  "model_gateway",
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
  "archiving",
  "archived",
  "released",
]);
const PULL_REQUEST_RELATIONS = new Set<CodePullRequestRelation>([
  "authored",
  "contributed",
]);
const PULL_REQUEST_FACT_STATES = new Set<string>(["open", "merged", "closed"]);
const TURN_STATUSES = new Set<CodeTurnStatus>([
  "queued",
  "running",
  "waiting",
  "cancelling",
  "waiting_for_client",
  "waiting_for_agent_run",
  "cancelling_client",
  "resuming",
  "retry_wait",
  "completed",
  "failed",
  "interrupted",
]);
const TURN_REWRITE_STATES = new Set<CodeTurnRewriteState>([
  "rewriting",
  "rewritten",
  "failed",
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
const ANALYTICS_RANGES = new Set<CodeAnalyticsRange>([
  "7d",
  "30d",
  "90d",
  "all",
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
    !nonEmptyLine(value.host) ||
    !nonEmptyLine(value.owner) ||
    !nonEmptyLine(value.name)
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
    !nonEmptyLine(value.host) ||
    !nonEmptyLine(value.owner) ||
    !nonEmptyLine(value.name) ||
    !nonEmptyLine(value.name_with_owner) ||
    !nonEmptyLine(value.url) ||
    !optionalLine(value.default_branch) ||
    !optionalWireId(value.tidebreak_repo_id)
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
    !optionalLine(value.viewer_login) ||
    !blockText(value.remediation)
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
    !nonEmptyLine(value.kind) ||
    !blockText(value.message) ||
    !optionalTimestamp(value.retry_at)
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
      "relation",
    ]) ||
    !wireId(value.workspace_id) ||
    !wireId(value.repo_id) ||
    !nonEmptyLine(value.title) ||
    !nonEmptyLine(value.branch_name) ||
    !isMember(value.status, WORKSPACE_STATUSES) ||
    typeof value.exact !== "boolean" ||
    (value.relation !== undefined &&
      !isMember(value.relation, PULL_REQUEST_RELATIONS))
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
    ...(value.relation !== undefined ? { relation: value.relation } : {}),
  };
}

function parseCodeWorkspacePullRequestFact(
  value: unknown,
): CodeWorkspacePullRequestFact | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspacePullRequestFact>(value, [
      "host",
      "repo_owner",
      "repo_name",
      "number",
      "url",
      "title",
      "state",
      "draft",
      "author",
      "head_branch",
      "base_branch",
      "head_sha",
      "relation",
      "created_at",
      "updated_at",
      "merged_at",
      "closed_at",
      "last_seen_at",
    ]) ||
    !nonEmptyLine(value.host) ||
    !nonEmptyLine(value.repo_owner) ||
    !nonEmptyLine(value.repo_name) ||
    !isFiniteNumber(value.number) ||
    !nonEmptyLine(value.url) ||
    !lineText(value.title) ||
    !PULL_REQUEST_FACT_STATES.has(value.state as string) ||
    typeof value.draft !== "boolean" ||
    !optionalLine(value.author) ||
    !lineText(value.head_branch) ||
    !lineText(value.base_branch) ||
    !optionalWireId(value.head_sha) ||
    !isMember(value.relation, PULL_REQUEST_RELATIONS) ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at) ||
    !optionalTimestamp(value.merged_at) ||
    !optionalTimestamp(value.closed_at) ||
    !timestamp(value.last_seen_at)
  ) {
    return null;
  }
  return {
    host: value.host,
    repo_owner: value.repo_owner,
    repo_name: value.repo_name,
    number: value.number,
    url: value.url,
    title: value.title,
    state: value.state as string,
    draft: value.draft,
    ...(value.author !== undefined ? { author: value.author } : {}),
    head_branch: value.head_branch,
    base_branch: value.base_branch,
    ...(value.head_sha !== undefined ? { head_sha: value.head_sha } : {}),
    relation: value.relation,
    created_at: value.created_at,
    updated_at: value.updated_at,
    ...(value.merged_at !== undefined ? { merged_at: value.merged_at } : {}),
    ...(value.closed_at !== undefined ? { closed_at: value.closed_at } : {}),
    last_seen_at: value.last_seen_at,
  };
}

export function parseCodeWorkspacePullRequests(
  value: unknown,
): CodeWorkspacePullRequests | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspacePullRequests>(value, ["items", "fetched_at"]) ||
    !Array.isArray(value.items) ||
    !timestamp(value.fetched_at)
  ) {
    return null;
  }
  const items: CodeWorkspacePullRequestFact[] = [];
  for (const item of value.items) {
    const parsed = parseCodeWorkspacePullRequestFact(item);
    if (!parsed) return null;
    items.push(parsed);
  }
  return { items, fetched_at: value.fetched_at };
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
    !nonEmptyLine(value.name) ||
    !isMember(value.bucket, DELIVERY_CHECK_BUCKETS) ||
    !optionalBlock(value.detail) ||
    !optionalLine(value.url) ||
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
      "in_merge_queue",
      "comment_count",
      "checks",
      "attention_reasons",
      "ready_to_merge",
      "workspace_links",
      "stack_parent_number",
      "stack_number",
      "stack_size",
      "unregistered_stack_numbers",
      "labels",
      "created_at",
      "updated_at",
      "merged_at",
      "closed_at",
    ]) ||
    !wireId(value.id) ||
    !isPositiveInteger(value.number) ||
    !nonEmptyLine(value.url) ||
    !nonEmptyLine(value.title) ||
    !nonEmptyLine(value.state) ||
    typeof value.draft !== "boolean" ||
    !optionalLine(value.author) ||
    !optionalLine(value.author_avatar_url) ||
    !nonEmptyLine(value.head_branch) ||
    !nonEmptyLine(value.base_branch) ||
    !optionalWireId(value.head_sha) ||
    !optionalLine(value.review_decision) ||
    !optionalLine(value.mergeable) ||
    !optionalLine(value.merge_state_status) ||
    typeof value.auto_merge_enabled !== "boolean" ||
    (value.in_merge_queue !== undefined &&
      typeof value.in_merge_queue !== "boolean") ||
    (value.comment_count !== undefined &&
      !isNonNegativeInteger(value.comment_count)) ||
    !Array.isArray(value.checks) ||
    !Array.isArray(value.attention_reasons) ||
    !value.attention_reasons.every((reason) =>
      isMember(reason, DELIVERY_PR_ATTENTION_REASONS),
    ) ||
    typeof value.ready_to_merge !== "boolean" ||
    (value.stack_parent_number !== undefined &&
      !isFiniteNumber(value.stack_parent_number)) ||
    (value.stack_number !== undefined && !isFiniteNumber(value.stack_number)) ||
    (value.stack_size !== undefined && !isFiniteNumber(value.stack_size)) ||
    (value.unregistered_stack_numbers !== undefined &&
      (!Array.isArray(value.unregistered_stack_numbers) ||
        !value.unregistered_stack_numbers.every(isFiniteNumber))) ||
    !Array.isArray(value.workspace_links) ||
    !lineList(value.labels) ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at) ||
    !optionalTimestamp(value.merged_at) ||
    !optionalTimestamp(value.closed_at)
  ) {
    return null;
  }
  const unregisteredStackNumbers = Array.isArray(
    value.unregistered_stack_numbers,
  )
    ? value.unregistered_stack_numbers.filter(
        (entry): entry is number => typeof entry === "number",
      )
    : undefined;
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
    ...(value.in_merge_queue !== undefined
      ? { in_merge_queue: value.in_merge_queue }
      : {}),
    ...(value.comment_count !== undefined
      ? { comment_count: value.comment_count }
      : {}),
    checks,
    attention_reasons: [...value.attention_reasons],
    ready_to_merge: value.ready_to_merge,
    workspace_links,
    ...(value.stack_parent_number !== undefined
      ? { stack_parent_number: value.stack_parent_number }
      : {}),
    ...(value.stack_number !== undefined
      ? { stack_number: value.stack_number }
      : {}),
    ...(value.stack_size !== undefined ? { stack_size: value.stack_size } : {}),
    ...(unregisteredStackNumbers !== undefined
      ? { unregistered_stack_numbers: unregisteredStackNumbers }
      : {}),
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
    !timestamp(value.fetched_at)
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
    !optionalCursor(value.next_cursor) ||
    !timestamp(value.fetched_at)
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
      "stack",
      "files",
      "files_truncated",
      "comments",
      "errors",
      "can_mark_ready",
      "can_merge",
      "can_rerun_failed",
      "can_close",
      "can_reopen",
      "can_comment",
    ]) ||
    !blockText(value.body) ||
    !lineList(value.labels) ||
    !lineList(value.assignees) ||
    !lineList(value.requested_reviewers) ||
    !isNonNegativeInteger(value.changed_files) ||
    !isNonNegativeInteger(value.additions) ||
    !isNonNegativeInteger(value.deletions) ||
    !isNonNegativeInteger(value.commits) ||
    !optionalLine(value.merged_by) ||
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
  const errors = parseDeliveryErrors(value.errors);
  if (!summary || !errors) return null;
  const stack: CodeDeliveryStackMember[] = [];
  if (value.stack !== undefined) {
    if (!Array.isArray(value.stack)) return null;
    for (const item of value.stack) {
      const member = parseCodeDeliveryStackMember(item);
      if (!member) return null;
      stack.push(member);
    }
  }
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
    errors,
    ...(stack.length > 0 ? { stack } : {}),
    can_mark_ready: value.can_mark_ready,
    can_merge: value.can_merge,
    can_rerun_failed: value.can_rerun_failed,
    can_close: value.can_close,
    can_reopen: value.can_reopen,
    can_comment: value.can_comment,
    ...(value.merged_by !== undefined ? { merged_by: value.merged_by } : {}),
  };
}

function parseCodeDeliveryStackMember(
  value: unknown,
): CodeDeliveryStackMember | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryStackMember>(value, [
      "number",
      "state",
      "draft",
      "merged_at",
      "head_branch",
      "head_sha",
    ]) ||
    !isPositiveInteger(value.number) ||
    !nonEmptyLine(value.state) ||
    typeof value.draft !== "boolean" ||
    !nonEmptyLine(value.head_branch) ||
    !optionalTimestamp(value.merged_at) ||
    !optionalWireId(value.head_sha)
  ) {
    return null;
  }
  return {
    number: value.number,
    state: value.state,
    draft: value.draft,
    head_branch: value.head_branch,
    ...(value.merged_at !== undefined ? { merged_at: value.merged_at } : {}),
    ...(value.head_sha !== undefined ? { head_sha: value.head_sha } : {}),
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
    !nonEmptyLine(value.path) ||
    !nonEmptyLine(value.status) ||
    !isNonNegativeInteger(value.additions) ||
    !isNonNegativeInteger(value.deletions) ||
    !optionalLine(value.previous_path) ||
    !optionalRaw(value.patch)
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
      "run_attempt",
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
    !wireId(value.id) ||
    !isMember(value.kind, DELIVERY_RUN_KINDS) ||
    !isPositiveInteger(value.github_id) ||
    !(
      value.run_attempt === undefined || isPositiveInteger(value.run_attempt)
    ) ||
    !nonEmptyLine(value.name) ||
    !nonEmptyLine(value.url) ||
    !nonEmptyLine(value.status) ||
    !optionalLine(value.conclusion) ||
    !optionalLine(value.workflow) ||
    !optionalLine(value.environment) ||
    !optionalLine(value.branch) ||
    !optionalWireId(value.sha) ||
    !optionalLine(value.event) ||
    !optionalLine(value.actor) ||
    !Array.isArray(value.attention_reasons) ||
    !value.attention_reasons.every((reason) =>
      isMember(reason, DELIVERY_RUN_ATTENTION_REASONS),
    ) ||
    !Array.isArray(value.workspace_links) ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at)
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
    ...(value.run_attempt !== undefined
      ? { run_attempt: value.run_attempt }
      : {}),
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
    !optionalCursor(value.next_cursor) ||
    !timestamp(value.fetched_at)
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
    !nonEmptyLine(value.name) ||
    !nonEmptyLine(value.status) ||
    !optionalLine(value.conclusion) ||
    !nonEmptyLine(value.url) ||
    !nullableTimestamp(value.started_at) ||
    !nullableTimestamp(value.completed_at) ||
    !lineList(value.failed_steps)
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
    !nonEmptyLine(value.state) ||
    !blockText(value.description) ||
    !optionalLine(value.environment_url) ||
    !optionalLine(value.log_url) ||
    !timestamp(value.created_at)
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
      "errors",
    ]) ||
    !Array.isArray(value.jobs) ||
    !Array.isArray(value.deployment_statuses) ||
    typeof value.can_rerun_failed !== "boolean"
  ) {
    return null;
  }
  const summary = parseCodeDeliveryRunSummary(value.summary);
  const errors = parseDeliveryErrors(value.errors);
  if (!summary || !errors) return null;
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
    errors,
  };
}

function parseCodeDeliveryRerunOutcome(
  value: unknown,
): NonNullable<CodeDeliveryActionResult["rerun_outcomes"]>[number] | null {
  if (
    !isRecord(value) ||
    !onlyKeys<NonNullable<CodeDeliveryActionResult["rerun_outcomes"]>[number]>(
      value,
      ["workflow_run_id", "success", "error"],
    ) ||
    !isPositiveInteger(value.workflow_run_id) ||
    typeof value.success !== "boolean" ||
    !optionalBlock(value.error)
  ) {
    return null;
  }
  return {
    workflow_run_id: value.workflow_run_id,
    success: value.success,
    ...(value.error !== undefined ? { error: value.error } : {}),
  };
}

export function parseCodeDeliveryActionResult(
  value: unknown,
): CodeDeliveryActionResult | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeDeliveryActionResult>(value, [
      "success",
      "message",
      "rerun_outcomes",
    ]) ||
    typeof value.success !== "boolean" ||
    !blockText(value.message) ||
    (value.rerun_outcomes !== undefined && !Array.isArray(value.rerun_outcomes))
  ) {
    return null;
  }
  const rerun_outcomes = [];
  for (const item of value.rerun_outcomes ?? []) {
    const outcome = parseCodeDeliveryRerunOutcome(item);
    if (!outcome) return null;
    rerun_outcomes.push(outcome);
  }
  return {
    success: value.success,
    message: value.message,
    ...(value.rerun_outcomes !== undefined ? { rerun_outcomes } : {}),
  };
}

export function parseCodeSubscriptionUsage(
  value: unknown,
): CodeSubscriptionUsage | null {
  if (
    !isRecord(value) ||
    !isMember(value.source, USAGE_SOURCES) ||
    !Array.isArray(value.providers) ||
    !lineList(value.diagnostics)
  ) {
    return null;
  }
  const providers: CodeSubscriptionUsage["providers"] = [];
  for (const provider of value.providers) {
    if (
      !isRecord(provider) ||
      !wireId(provider.id) ||
      !nonEmptyLine(provider.label) ||
      !Array.isArray(provider.accounts)
    ) {
      return null;
    }
    const accounts = [];
    for (const account of provider.accounts) {
      if (
        !isRecord(account) ||
        !wireId(account.id) ||
        !nonEmptyLine(account.label) ||
        typeof account.is_own !== "boolean" ||
        !lineText(account.state) ||
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
          !wireId(window.key) ||
          !nonEmptyLine(window.label) ||
          !isFiniteNumber(window.used_percent) ||
          (window.resets_at_unix_seconds !== undefined &&
            !isFiniteNumber(window.resets_at_unix_seconds)) ||
          !optionalLine(window.status) ||
          !optionalLine(window.model_scope)
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

function parseCodeAnalyticsTotals(value: unknown): CodeAnalyticsTotals | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsTotals>(value, [
      "sessions",
      "turns",
      "completed_turns",
      "failed_turns",
      "interrupted_turns",
      "running_turns",
      "input_tokens",
      "output_tokens",
      "cache_read_tokens",
      "cache_write_tokens",
      "total_tokens",
      "estimated_cost_microusd",
      "pull_requests_opened",
      "pull_requests_merged",
    ]) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.completed_turns) ||
    !isNonNegativeInteger(value.failed_turns) ||
    !isNonNegativeInteger(value.interrupted_turns) ||
    !isNonNegativeInteger(value.running_turns) ||
    !isNonNegativeInteger(value.input_tokens) ||
    !isNonNegativeInteger(value.output_tokens) ||
    !isNonNegativeInteger(value.cache_read_tokens) ||
    !isNonNegativeInteger(value.cache_write_tokens) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    !isNonNegativeInteger(value.pull_requests_opened) ||
    !isNonNegativeInteger(value.pull_requests_merged)
  ) {
    return null;
  }
  return value as CodeAnalyticsTotals;
}

function parseCodeAnalyticsDay(value: unknown): CodeAnalyticsDay | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsDay>(value, [
      "date",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
      "pull_requests_opened",
      "pull_requests_merged",
    ]) ||
    !timestamp(value.date) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    !isNonNegativeInteger(value.pull_requests_opened) ||
    !isNonNegativeInteger(value.pull_requests_merged)
  ) {
    return null;
  }
  return value as CodeAnalyticsDay;
}

function parseCodeAnalyticsRepository(
  value: unknown,
): CodeAnalyticsRepository | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsRepository>(value, [
      "repo_id",
      "name",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
      "pull_requests_opened",
      "pull_requests_merged",
    ]) ||
    !wireId(value.repo_id) ||
    !nonEmptyLine(value.name) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    !isNonNegativeInteger(value.pull_requests_opened) ||
    !isNonNegativeInteger(value.pull_requests_merged)
  ) {
    return null;
  }
  return value as CodeAnalyticsRepository;
}

function parseCodeAnalyticsModel(value: unknown): CodeAnalyticsModel | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsModel>(value, [
      "model_id",
      "harness_kind",
      "fast_mode",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
      "priced",
    ]) ||
    !optionalLine(value.model_id) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    typeof value.fast_mode !== "boolean" ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd) ||
    typeof value.priced !== "boolean"
  ) {
    return null;
  }
  return value as CodeAnalyticsModel;
}

function parseCodeAnalyticsHarness(
  value: unknown,
): CodeAnalyticsHarness | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsHarness>(value, [
      "harness_kind",
      "sessions",
      "turns",
      "total_tokens",
      "estimated_cost_microusd",
    ]) ||
    !isMember(value.harness_kind, HARNESS_KINDS) ||
    !isNonNegativeInteger(value.sessions) ||
    !isNonNegativeInteger(value.turns) ||
    !isNonNegativeInteger(value.total_tokens) ||
    !isNonNegativeInteger(value.estimated_cost_microusd)
  ) {
    return null;
  }
  return value as CodeAnalyticsHarness;
}

function parseCodeAnalyticsPricing(
  value: unknown,
): CodeAnalyticsPricingCoverage | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsPricingCoverage>(value, [
      "priced_turns",
      "unpriced_turns",
      "priced_tokens",
      "unpriced_tokens",
      "prices_as_of",
    ]) ||
    !isNonNegativeInteger(value.priced_turns) ||
    !isNonNegativeInteger(value.unpriced_turns) ||
    !isNonNegativeInteger(value.priced_tokens) ||
    !isNonNegativeInteger(value.unpriced_tokens) ||
    !timestamp(value.prices_as_of)
  ) {
    return null;
  }
  return value as CodeAnalyticsPricingCoverage;
}

export function parseCodeAnalytics(
  value: unknown,
): CodeAnalyticsSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeAnalyticsSnapshot>(value, [
      "range",
      "from",
      "through",
      "repo_id",
      "totals",
      "daily",
      "repositories",
      "models",
      "harnesses",
      "pricing",
    ]) ||
    !isMember(value.range, ANALYTICS_RANGES) ||
    !optionalTimestamp(value.from) ||
    !timestamp(value.through) ||
    !optionalWireId(value.repo_id) ||
    !Array.isArray(value.daily) ||
    !Array.isArray(value.repositories) ||
    !Array.isArray(value.models) ||
    !Array.isArray(value.harnesses)
  ) {
    return null;
  }
  const totals = parseCodeAnalyticsTotals(value.totals);
  const pricing = parseCodeAnalyticsPricing(value.pricing);
  const daily = value.daily.map(parseCodeAnalyticsDay);
  const repositories = value.repositories.map(parseCodeAnalyticsRepository);
  const models = value.models.map(parseCodeAnalyticsModel);
  const harnesses = value.harnesses.map(parseCodeAnalyticsHarness);
  if (
    !totals ||
    !pricing ||
    daily.some((item) => item === null) ||
    repositories.some((item) => item === null) ||
    models.some((item) => item === null) ||
    harnesses.some((item) => item === null)
  ) {
    return null;
  }
  return {
    range: value.range,
    through: value.through,
    totals,
    daily: daily as CodeAnalyticsDay[],
    repositories: repositories as CodeAnalyticsRepository[],
    models: models as CodeAnalyticsModel[],
    harnesses: harnesses as CodeAnalyticsHarness[],
    pricing,
    ...(value.from !== undefined ? { from: value.from } : {}),
    ...(value.repo_id !== undefined ? { repo_id: value.repo_id } : {}),
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
    !wireId(value.id) ||
    !lineText(value.phase) ||
    typeof value.done !== "boolean" ||
    (value.percent !== undefined && !isFiniteNumber(value.percent)) ||
    !optionalBlock(value.error) ||
    !optionalWireId(value.repo_id)
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
    !lineText(value.phase) ||
    typeof value.done !== "boolean" ||
    !optionalLine(value.version) ||
    !optionalBlock(value.error)
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
    !optionalLine(value.parent_dir) ||
    typeof value.gh_found !== "boolean" ||
    !blockText(value.gh_remediation) ||
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

/**
 * One source the machine reports.
 *
 * An unfamiliar `kind` parses fine and is dropped by the caller rather than
 * rejected here: the set of sources is the machine's to grow, and a client
 * that refused the whole envelope over one unknown member could never be
 * older than its machine.
 */
function parseCodeRepoSource(value: unknown): CodeRepoSource | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeRepoSource>(value, [
      "kind",
      "available",
      "remediation",
    ]) ||
    !lineText(value.kind) ||
    typeof value.available !== "boolean" ||
    !optionalBlock(value.remediation)
  ) {
    return null;
  }
  return {
    kind: value.kind,
    available: value.available,
    ...(value.remediation !== undefined
      ? { remediation: value.remediation }
      : {}),
  };
}

export function parseCodeRepoSources(value: unknown): CodeRepoSources | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeRepoSources>(value, ["sources", "chooses_destination"]) ||
    !Array.isArray(value.sources) ||
    typeof value.chooses_destination !== "boolean"
  ) {
    return null;
  }
  const sources: CodeRepoSource[] = [];
  for (const entry of value.sources) {
    const source = parseCodeRepoSource(entry);
    if (!source) return null;
    sources.push(source);
  }
  return { sources, chooses_destination: value.chooses_destination };
}

function parseCodeGithubRepository(
  value: unknown,
): WireCodeGithubRepository | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeGithubRepository>(value, [
      "full_name",
      "private",
      "description",
    ]) ||
    !lineText(value.full_name) ||
    typeof value.private !== "boolean" ||
    !optionalBlock(value.description)
  ) {
    return null;
  }
  return {
    full_name: value.full_name,
    private: value.private,
    ...(value.description !== undefined
      ? { description: value.description }
      : {}),
  };
}

export function parseCodeGithubRepositories(
  value: unknown,
): WireCodeGithubRepositories | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeGithubRepositories>(value, ["repositories"]) ||
    !Array.isArray(value.repositories)
  ) {
    return null;
  }
  const repositories: WireCodeGithubRepository[] = [];
  for (const entry of value.repositories) {
    const repository = parseCodeGithubRepository(entry);
    if (!repository) return null;
    repositories.push(repository);
  }
  return { repositories };
}

export function parseCodeForkTranscript(
  value: unknown,
): CodeForkTranscript | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeForkTranscript>(value, [
      "path",
      "dir",
      "byte_len",
      "turns",
      "total_turns",
      "at_turn_ordinal",
      "truncated",
    ]) ||
    !lineText(value.path) ||
    !lineText(value.dir) ||
    typeof value.byte_len !== "number" ||
    typeof value.turns !== "number" ||
    typeof value.total_turns !== "number" ||
    (value.at_turn_ordinal !== undefined &&
      !isFiniteNumber(value.at_turn_ordinal)) ||
    typeof value.truncated !== "boolean"
  ) {
    return null;
  }
  return {
    path: value.path,
    dir: value.dir,
    byte_len: value.byte_len,
    turns: value.turns,
    total_turns: value.total_turns,
    ...(value.at_turn_ordinal !== undefined
      ? { at_turn_ordinal: value.at_turn_ordinal }
      : {}),
    truncated: value.truncated,
  };
}

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

export function parseCodeWorktreeRoot(value: unknown): CodeWorktreeRoot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorktreeRoot>(value, [
      "root",
      "effective_root",
      "default_root",
    ]) ||
    !optionalLine(value.root) ||
    !lineText(value.effective_root) ||
    !lineText(value.default_root)
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
    !wireId(value.id) ||
    !nonEmptyLine(value.root_path) ||
    !nonEmptyLine(value.display_name) ||
    !nonEmptyLine(value.default_base_ref) ||
    !nonEmptyLine(value.branch_prefix) ||
    !timestamp(value.created_at) ||
    !optionalBlock(value.setup_script) ||
    !optionalBlock(value.archive_script) ||
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
    !nonEmptyLine(value.name) ||
    !blockText(value.command) ||
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
      "released_at",
      "released_tip",
      "bundle_bytes",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.repo_id) ||
    !nonEmptyLine(value.title) ||
    !nonEmptyLine(value.worktree_path) ||
    !nonEmptyLine(value.branch_name) ||
    !nonEmptyLine(value.base_ref) ||
    !isMember(value.status, WORKSPACE_STATUSES) ||
    !timestamp(value.created_at) ||
    !optionalTimestamp(value.archived_at) ||
    !optionalTimestamp(value.released_at) ||
    !optionalWireId(value.released_tip) ||
    (value.bundle_bytes !== undefined && !isFiniteNumber(value.bundle_bytes))
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
    ...(value.released_at !== undefined
      ? { released_at: value.released_at }
      : {}),
    ...(value.released_tip !== undefined
      ? { released_tip: value.released_tip }
      : {}),
    ...(value.bundle_bytes !== undefined
      ? { bundle_bytes: value.bundle_bytes }
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

function parsePullRequestChecks(
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
      "dirty",
      "unpushed",
      "ahead",
      "has_upstream",
      "suggested_commit_message",
      "pr",
      "gh_found",
      "gh_authenticated",
      "remediation",
      "pushes_as",
      "pushes_as_self",
      "watch",
    ]) ||
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

function parsePullRequestComment(value: unknown): PullRequestComment | null {
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
      "external_origin",
    ]) ||
    !wireId(value.id) ||
    !nullableWireId(value.workspace_id) ||
    !isMember(value.kind, SESSION_KINDS) ||
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
    ...(external_origin !== undefined ? { external_origin } : {}),
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
      "model",
      "fast_mode",
      "user_input",
      "attachments",
      "usage",
      "checkpoint_ref",
      "diffstat",
      "started_at",
      "ended_at",
      "rewrite",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.session_id) ||
    !isFiniteNumber(value.ordinal) ||
    !isMember(value.status, TURN_STATUSES) ||
    !optionalLine(value.model) ||
    // Serialized unconditionally, but tolerate its absence: a turn row
    // written before this snapshot existed reads as off, which is what
    // it was.
    (value.fast_mode !== undefined && typeof value.fast_mode !== "boolean") ||
    !optionalLine(value.checkpoint_ref) ||
    !rawText(value.user_input) ||
    !timestamp(value.started_at) ||
    !optionalTimestamp(value.ended_at) ||
    !optionalBlock(value.rewrite)
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
    fast_mode: value.fast_mode === true,
    ...(value.model !== undefined ? { model: value.model } : {}),
    user_input: value.user_input,
    started_at: value.started_at,
    attachments,
    ...(usage ? { usage } : {}),
    ...(value.checkpoint_ref !== undefined
      ? { checkpoint_ref: value.checkpoint_ref }
      : {}),
    ...(diffstat ? { diffstat } : {}),
    ...(value.ended_at !== undefined ? { ended_at: value.ended_at } : {}),
    ...(value.rewrite !== undefined ? { rewrite: value.rewrite } : {}),
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
      !wireId(item.blob_id) ||
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
      "id",
      "session_id",
      "message",
      "position",
      "created_at",
      "updated_at",
    ]) ||
    !wireId(value.id) ||
    !wireId(value.session_id) ||
    !rawText(value.message) ||
    !isFiniteNumber(value.position) ||
    !timestamp(value.created_at) ||
    !timestamp(value.updated_at)
  ) {
    return null;
  }
  return {
    id: value.id,
    session_id: value.session_id,
    message: value.message,
    position: value.position,
    created_at: value.created_at,
    updated_at: value.updated_at,
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
    if (!nonEmptyLine(item)) return null;
    paths.push(item);
  }
  return { paths, truncated: value.truncated };
}

export function parseCodeWorkspaceSearch(
  value: unknown,
): CodeWorkspaceSearch | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeWorkspaceSearch>(value, [
      "matches",
      "history_matches",
      "truncated",
    ]) ||
    !Array.isArray(value.matches) ||
    (value.history_matches !== undefined &&
      !Array.isArray(value.history_matches)) ||
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
      !nonEmptyLine(item.path) ||
      !isFiniteNumber(item.line_number) ||
      item.line_number < 1 ||
      !rawText(item.line)
    ) {
      return null;
    }
    matches.push({
      path: item.path,
      line_number: item.line_number,
      line: item.line,
    });
  }
  const historyMatches: WireCodeWorkspaceHistorySearchMatch[] = [];
  for (const item of value.history_matches ?? []) {
    if (
      !isRecord(item) ||
      !onlyKeys<WireCodeWorkspaceHistorySearchMatch>(item, [
        "workspace_id",
        "workspace_title",
        "session_id",
        "turn_id",
        "source",
        "preview",
        "created_at",
      ]) ||
      !wireId(item.workspace_id) ||
      !nonEmptyLine(item.workspace_title) ||
      !wireId(item.session_id) ||
      (item.turn_id !== undefined && !wireId(item.turn_id)) ||
      !["turn_user_input", "turn_narrative", "event"].includes(
        item.source as string,
      ) ||
      !rawText(item.preview) ||
      !timestamp(item.created_at)
    ) {
      return null;
    }
    historyMatches.push({
      workspace_id: item.workspace_id,
      workspace_title: item.workspace_title,
      session_id: item.session_id,
      ...(item.turn_id === undefined ? {} : { turn_id: item.turn_id }),
      source: item.source as WireCodeWorkspaceHistorySearchMatch["source"],
      preview: item.preview,
      created_at: item.created_at,
    });
  }
  return {
    matches,
    ...(value.history_matches === undefined
      ? {}
      : { history_matches: historyMatches }),
    truncated: value.truncated,
  };
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
  if (value.turn_id !== undefined && !wireId(value.turn_id)) return null;
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
    !lineText(value.path) ||
    !rawText(value.content) ||
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
    !rawText(value.diff) ||
    typeof value.truncated !== "boolean" ||
    !optionalWireId(value.turn_id) ||
    !optionalLine(value.file)
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
    !lineText(value.path) ||
    !isMember(value.kind, FILE_CHANGE_KINDS) ||
    !isFiniteNumber(value.insertions) ||
    !isFiniteNumber(value.deletions) ||
    !optionalLine(value.previous_path)
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
    !wireId(value.id) ||
    !wireId(value.workspace_id) ||
    !isFiniteNumber(value.cols) ||
    !isFiniteNumber(value.rows) ||
    typeof value.ended !== "boolean" ||
    !timestamp(value.created_at)
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
    !wireId(value.id) ||
    !wireId(value.workspace_id) ||
    !rawText(value.bytes) ||
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

export type ParsedHarnessModelList = {
  kind: HarnessKind;
  /** Missing means an older server whose listing was always native. */
  source?: WireHarnessModelSource;
  models: ParsedHarnessModel[];
  reasoning_efforts: ReasoningEffort[];
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

export function parseHarnessModelList(
  value: unknown,
): ParsedHarnessModelList | null {
  if (
    !isRecord(value) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    !Array.isArray(value.models)
  ) {
    return null;
  }
  const source =
    value.source === undefined
      ? "harness"
      : isMember(value.source, HARNESS_MODEL_SOURCES)
        ? value.source
        : null;
  if (source === null) return null;
  const models: ParsedHarnessModel[] = [];
  for (const item of value.models) {
    if (
      !isRecord(item) ||
      !lineText(item.id) ||
      !lineText(item.label) ||
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
    source,
    models,
    reasoning_efforts: parseEfforts(value.reasoning_efforts),
  };
}

export function parseHarnessDoctorReport(
  value: unknown,
): HarnessDoctorReport | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessDoctorReport>(value, [
      "harnesses",
      "update_channel",
    ]) ||
    !Array.isArray(value.harnesses) ||
    (value.update_channel !== undefined &&
      !isMember(value.update_channel, HARNESS_UPDATE_CHANNELS))
  ) {
    return null;
  }
  const harnesses: HarnessDoctorEntry[] = [];
  for (const entry of value.harnesses) {
    const parsed = parseHarnessDoctorEntry(entry);
    if (!parsed) return null;
    harnesses.push(parsed);
  }
  // A server that predates the channel drives its pins and nothing else.
  return { harnesses, update_channel: value.update_channel ?? "pinned" };
}

export function parseHarnessDoctorEntry(
  value: unknown,
): HarnessDoctorEntry | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireHarnessDoctorEntry>(value, [
      "kind",
      "found",
      "installable",
      "path",
      "version",
      "tier",
      "caps",
      "commands",
      "authenticated",
      "auth_mode",
      "remediation",
      "stderr",
      "unrecognized_event_count",
      "relaunch_composes_permission_mode",
      "pinned_version",
      "managed_version",
      "latest_version",
      "update_available",
    ]) ||
    !isMember(value.kind, HARNESS_KINDS) ||
    typeof value.found !== "boolean" ||
    !optionalLine(value.path) ||
    !optionalLine(value.version) ||
    !optionalLine(value.pinned_version) ||
    !optionalLine(value.managed_version) ||
    !optionalLine(value.latest_version) ||
    (value.update_available !== undefined &&
      typeof value.update_available !== "boolean") ||
    !isMember(value.tier, HARNESS_TIERS) ||
    (value.authenticated !== undefined &&
      typeof value.authenticated !== "boolean") ||
    (value.auth_mode !== undefined &&
      !isMember(value.auth_mode, HARNESS_AUTH_MODES)) ||
    !blockText(value.remediation) ||
    !rawText(value.stderr) ||
    !isFiniteNumber(value.unrecognized_event_count) ||
    (value.relaunch_composes_permission_mode !== undefined &&
      typeof value.relaunch_composes_permission_mode !== "boolean")
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
    // A server that predates the field offers no on-demand download, which
    // is what this build did before the pin became lazy.
    installable: value.installable === true,
    // Ditto the hosted doctor: a server that predates it knows only the
    // local sign-in probe, and that is the local answer.
    auth_mode: value.auth_mode ?? "local_sign_in",
    // Engines historically recomposed the mode on relaunch; a server that
    // predates the field still behaves that way.
    relaunch_composes_permission_mode:
      value.relaunch_composes_permission_mode !== false,
    tier: value.tier,
    caps,
    remediation: value.remediation,
    stderr: value.stderr,
    unrecognized_event_count: value.unrecognized_event_count,
    commands,
    // A server that predates the update channel never has one to offer.
    update_available: value.update_available === true,
    ...(value.path !== undefined ? { path: value.path } : {}),
    ...(value.version !== undefined ? { version: value.version } : {}),
    ...(value.pinned_version !== undefined
      ? { pinned_version: value.pinned_version }
      : {}),
    ...(value.managed_version !== undefined
      ? { managed_version: value.managed_version }
      : {}),
    ...(value.latest_version !== undefined
      ? { latest_version: value.latest_version }
      : {}),
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
      !lineText(item.name) ||
      item.name.length === 0 ||
      !blockText(item.description)
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
      "durable_parks",
      "user_questions",
      "standing_grants",
      "mid_turn_resume",
      "transcript",
      "memory_loopback",
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
    !isMember(value.slash_commands, CAP_LEVELS) ||
    !isMember(value.durable_parks, CAP_LEVELS) ||
    !isMember(value.user_questions, CAP_LEVELS) ||
    !isMember(value.standing_grants, CAP_LEVELS) ||
    !isMember(value.mid_turn_resume, CAP_LEVELS) ||
    !isMember(value.transcript, CAP_LEVELS) ||
    !isMember(value.memory_loopback, CAP_LEVELS)
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
    durable_parks: value.durable_parks,
    user_questions: value.user_questions,
    standing_grants: value.standing_grants,
    mid_turn_resume: value.mid_turn_resume,
    transcript: value.transcript,
    memory_loopback: value.memory_loopback,
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
        !isRecord(value.refusal.details) ||
        typeof value.refusal.partial_output !== "boolean" ||
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
    case "approval_requested":
      // The internal engine journals the card's request beside the id so
      // the chat surface replays it; the code surface loads the row.
      if (
        !onlyKeys(value, ["type", "approval_id", "request"]) ||
        !wireId(value.approval_id)
      ) {
        return null;
      }
      return { type: "approval_requested", approval_id: value.approval_id };
    case "approval_resolved":
      if (
        !onlyKeys(value, ["type", "approval_id", "decision"]) ||
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
  const firstCallContextTokens = value.first_call_context_tokens;
  if (
    firstCallContextTokens !== undefined &&
    !isFiniteNumber(firstCallContextTokens)
  ) {
    return null;
  }
  return {
    input_tokens: value.input_tokens,
    output_tokens: value.output_tokens,
    cache_read_input_tokens: value.cache_read_input_tokens,
    cache_creation_input_tokens: value.cache_creation_input_tokens,
    context_tokens: contextTokens ?? 0,
    ...(firstCallContextTokens === undefined
      ? {}
      : { first_call_context_tokens: firstCallContextTokens }),
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

export function parseCodeSessionDigest(
  value: unknown,
): CodeSessionDigest | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeSessionDigest>(value, [
      "workspace",
      "session",
      "kind",
      "harness_kind",
      "lifecycle",
      "attention",
      "title",
      "turn_count",
      "trigger_target_at",
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
    !optionalBlock(value.recap)
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
    ...(value.harness_kind !== undefined
      ? { harness_kind: value.harness_kind }
      : {}),
    lifecycle: value.lifecycle,
    attention,
    title: value.title,
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
          "title",
          "turn_count",
          "trigger_target_at",
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
        !optionalBlock(value.recap)
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
        ...(value.harness_kind !== undefined
          ? { harness_kind: value.harness_kind }
          : {}),
        lifecycle: value.lifecycle,
        attention,
        title: value.title,
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

export function parseCodeApproval(value: unknown): CodeApprovalSnapshot | null {
  if (
    !isRecord(value) ||
    !wireId(value.id) ||
    !wireId(value.session_id) ||
    !wireId(value.turn_id) ||
    !isRecord(value.kind) ||
    typeof value.kind.type !== "string" ||
    !rawText(value.harness_raw_json) ||
    !isMember(value.state, APPROVAL_STATES) ||
    !timestamp(value.requested_at) ||
    !optionalBlock(value.feedback) ||
    !optionalTimestamp(value.decided_at)
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

export function parseCodeGrant(value: unknown): CodeGrantSnapshot | null {
  if (
    !isRecord(value) ||
    !wireId(value.id) ||
    !nonEmptyLine(value.channel_kind) ||
    !nonEmptyLine(value.external_identity) ||
    !optionalLine(value.display_name) ||
    !nonEmptyLine(value.workspace_identity) ||
    !optionalLine(value.workspace_name) ||
    !optionalLine(value.avatar_url) ||
    !timestamp(value.created_at) ||
    !optionalTimestamp(value.rotated_at) ||
    !optionalTimestamp(value.revoked_at) ||
    !optionalBlock(value.revoked_reason)
  ) {
    return null;
  }
  return {
    id: value.id,
    channel_kind: value.channel_kind,
    external_identity: value.external_identity,
    ...(value.display_name !== undefined
      ? { display_name: value.display_name }
      : {}),
    workspace_identity: value.workspace_identity,
    ...(value.workspace_name !== undefined
      ? { workspace_name: value.workspace_name }
      : {}),
    ...(value.avatar_url !== undefined ? { avatar_url: value.avatar_url } : {}),
    created_at: value.created_at,
    ...(value.rotated_at !== undefined ? { rotated_at: value.rotated_at } : {}),
    ...(value.revoked_at !== undefined ? { revoked_at: value.revoked_at } : {}),
    ...(value.revoked_reason !== undefined
      ? { revoked_reason: value.revoked_reason }
      : {}),
  };
}

export function parseCodeGrantList(value: unknown): CodeGrantSnapshot[] | null {
  if (!Array.isArray(value)) return null;
  const grants: CodeGrantSnapshot[] = [];
  for (const entry of value) {
    const grant = parseCodeGrant(entry);
    if (!grant) return null;
    grants.push(grant);
  }
  return grants;
}

export function parseCodeConnectPage(value: unknown): CodeConnectPage | null {
  if (
    !isRecord(value) ||
    !nonEmptyLine(value.channel_kind) ||
    !nonEmptyLine(value.display_name) ||
    !nonEmptyLine(value.workspace_name) ||
    !optionalLine(value.avatar_url) ||
    !nonEmptyLine(value.state) ||
    !wireId(value.csrf) ||
    !timestamp(value.expires_at)
  ) {
    return null;
  }
  return {
    channel_kind: value.channel_kind,
    display_name: value.display_name,
    workspace_name: value.workspace_name,
    ...(value.avatar_url !== undefined ? { avatar_url: value.avatar_url } : {}),
    state: value.state,
    csrf: value.csrf,
    expires_at: value.expires_at,
  };
}
