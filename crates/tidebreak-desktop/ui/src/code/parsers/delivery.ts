import {
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isPositiveInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
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
  PullRequestComment,
} from "../../api/types";
import type {
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
  CodeGitHubCapability as WireCodeGitHubCapability,
  CodeGitHubRepositoryRef as WireCodeGitHubRepositoryRef,
  CodeGitHubRepositoryTarget as WireCodeGitHubRepositoryTarget,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  lineList,
  blockText,
  optionalBlock,
  optionalRaw,
  wireId,
  optionalWireId,
  timestamp,
  optionalTimestamp,
  nullableTimestamp,
  optionalCursor,
  WORKSPACE_STATUSES,
} from "./shared";
import { parsePullRequestComment } from "./workspaces";

const PULL_REQUEST_RELATIONS = new Set<CodePullRequestRelation>([
  "authored",
  "contributed",
]);
const PULL_REQUEST_FACT_STATES = new Set<string>(["open", "merged", "closed"]);

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
