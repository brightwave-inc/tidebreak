import {
  isFiniteNumber,
  isMember,
  isNonNegativeInteger,
  isRecord,
  onlyKeys,
} from "../../lib/wireDecode";
import type {
  CodeRepoSnapshot,
  HarnessKind,
  CodeCloneDefaults,
  CodeRepoSource,
  CodeRepoSources,
  CodeCloneJobSnapshot,
  CodeWorktreeRoot,
  CodeProjectConfigEffect,
  CodeProjectConfigEffectKind,
  CodeProjectConfigFile,
  CodeRepoTrust,
  CodeRepoTrustSnapshot,
} from "../../api/types";
import type {
  CodeRepoSnapshot as WireCodeRepoSnapshot,
  CodeRepoTrustSnapshot as WireCodeRepoTrustSnapshot,
  CodeProjectConfigEffect as WireCodeProjectConfigEffect,
  CodeProjectConfigFile as WireCodeProjectConfigFile,
  QuickAction as WireQuickAction,
  CodeCloneDefaults as WireCodeCloneDefaults,
  CodeRepoSource as WireCodeRepoSource,
  CodeRepoSources as WireCodeRepoSources,
  CodeGithubRepositories as WireCodeGithubRepositories,
  CodeGithubRepository as WireCodeGithubRepository,
  CodeCloneJobSnapshot as WireCodeCloneJobSnapshot,
  CodeWorktreeRoot as WireCodeWorktreeRoot,
} from "../../generated/wire";
import {
  lineText,
  nonEmptyLine,
  optionalLine,
  blockText,
  optionalBlock,
  wireId,
  optionalWireId,
  timestamp,
  HARNESS_KINDS,
} from "./shared";

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

const REPO_TRUSTS = new Set<CodeRepoTrust>([
  "undecided",
  "trusted",
  "untrusted",
]);

const PROJECT_CONFIG_EFFECT_KINDS = new Set<CodeProjectConfigEffectKind>([
  "hooks",
  "mcp_servers",
  "plugins",
  "packages",
  "environment_variables",
  "permission_rules",
  "helper_commands",
  "custom_tools",
  "workflows",
  "scheduled_tasks",
  "agents",
  "commands",
  "skills",
  "instructions",
  "settings",
]);

/**
 * The most config files one checkout lists. The server reads a fixed set of
 * paths, well under this, so a longer list is not a server answer.
 */
const MAX_PROJECT_CONFIG_FILES = 128;

function parseProjectConfigEffect(
  value: unknown,
): CodeProjectConfigEffect | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeProjectConfigEffect>(value, ["kind", "count"]) ||
    !isMember(value.kind, PROJECT_CONFIG_EFFECT_KINDS) ||
    !isNonNegativeInteger(value.count)
  ) {
    return null;
  }
  return { kind: value.kind, count: value.count };
}

function parseProjectConfigFile(value: unknown): CodeProjectConfigFile | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeProjectConfigFile>(value, [
      "path",
      "engines",
      "effects",
    ]) ||
    !nonEmptyLine(value.path) ||
    !Array.isArray(value.engines) ||
    !Array.isArray(value.effects) ||
    value.effects.length > PROJECT_CONFIG_EFFECT_KINDS.size
  ) {
    return null;
  }
  const engines: HarnessKind[] = [];
  for (const engine of value.engines) {
    if (!isMember(engine, HARNESS_KINDS) || engines.includes(engine)) {
      return null;
    }
    engines.push(engine);
  }
  const effects: CodeProjectConfigEffect[] = [];
  for (const entry of value.effects) {
    const effect = parseProjectConfigEffect(entry);
    if (!effect) return null;
    effects.push(effect);
  }
  return { path: value.path, engines, effects };
}

/**
 * `GET /code/repos/{id}/trust`, its `PUT`, and
 * `GET /code/workspaces/{id}/trust`: whether engines load the repository's
 * own config, and the engine config the scanned checkout carries.
 */
export function parseCodeRepoTrust(
  value: unknown,
): CodeRepoTrustSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<WireCodeRepoTrustSnapshot>(value, [
      "repo_id",
      "trust",
      "files",
    ]) ||
    !wireId(value.repo_id) ||
    !isMember(value.trust, REPO_TRUSTS) ||
    !Array.isArray(value.files) ||
    value.files.length > MAX_PROJECT_CONFIG_FILES
  ) {
    return null;
  }
  const files: CodeProjectConfigFile[] = [];
  for (const entry of value.files) {
    const file = parseProjectConfigFile(entry);
    if (!file) return null;
    files.push(file);
  }
  return { repo_id: value.repo_id, trust: value.trust, files };
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
