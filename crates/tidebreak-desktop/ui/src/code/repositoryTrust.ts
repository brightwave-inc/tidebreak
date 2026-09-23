import type {
  CodeProjectConfigEffect,
  CodeProjectConfigEffectKind,
  CodeProjectConfigFile,
  CodeRepoTrustSnapshot,
  CodeWorkspaceSnapshot,
  HarnessKind,
} from "@/api/types";
import { HARNESS_LABELS } from "./labels";

/** One noun per effect, singular and plural. */
const EFFECT_NOUNS: Record<
  CodeProjectConfigEffectKind,
  readonly [one: string, many: string]
> = {
  hooks: ["hook", "hooks"],
  mcp_servers: ["MCP server", "MCP servers"],
  plugins: ["plugin", "plugins"],
  packages: ["package to install", "packages to install"],
  environment_variables: ["environment variable", "environment variables"],
  permission_rules: ["permission rule", "permission rules"],
  helper_commands: ["helper command", "helper commands"],
  custom_tools: ["custom tool", "custom tools"],
  workflows: ["workflow", "workflows"],
  scheduled_tasks: ["scheduled task", "scheduled tasks"],
  agents: ["agent", "agents"],
  commands: ["command", "commands"],
  skills: ["skill", "skills"],
  instructions: ["instruction file", "instruction files"],
  settings: ["other setting", "other settings"],
};

function effectLabel(
  effect: CodeProjectConfigEffect,
  directory: boolean,
): string {
  // A file like CLAUDE.md is the instructions; a rules directory holds some.
  if (effect.kind === "instructions" && !directory) return "instructions";
  const [one, many] = EFFECT_NOUNS[effect.kind];
  return `${effect.count} ${effect.count === 1 ? one : many}`;
}

/**
 * What loading one config file would do, as one line:
 * "2 hooks, 1 environment variable". A file the server could not summarize
 * still loads, so it says so rather than claiming nothing.
 */
export function projectConfigSummary(file: CodeProjectConfigFile): string {
  if (file.effects.length === 0) return "Engine settings";
  const text = file.effects
    .map((effect) => effectLabel(effect, file.path.endsWith("/")))
    .join(", ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** The engines that load a file, by name: "Claude Code, opencode". */
export function projectConfigEngines(file: CodeProjectConfigFile): string {
  return file.engines.map((engine) => HARNESS_LABELS[engine]).join(", ");
}

/** Whether a session on `harness` would load any of these files. */
export function projectConfigLoadsFor(
  files: readonly CodeProjectConfigFile[],
  harness: HarnessKind,
): boolean {
  return files.some((file) => file.engines.includes(harness));
}

/**
 * Whether starting a session on `harness` should ask about trust first:
 * nobody has decided for this repository, and its checkout carries config
 * that engine would load. Every other case starts at once, because the
 * server launches an undecided repository without its config anyway.
 */
export function needsTrustDecision(
  snapshot: CodeRepoTrustSnapshot,
  harness: HarnessKind,
): boolean {
  return (
    snapshot.trust === "undecided" &&
    projectConfigLoadsFor(snapshot.files, harness)
  );
}

/**
 * Whether a workspace's repository trust can be read from here: a sandbox
 * workspace keeps its checkout in the sandbox, and a reader who does not own
 * the workspace does not decide what its sessions load.
 */
export function workspaceHasLocalTrust(
  workspace: Pick<CodeWorkspaceSnapshot, "worktree_path" | "is_owner">,
): boolean {
  if (workspace.is_owner === false) return false;
  const path = workspace.worktree_path;
  return path !== "" && !path.startsWith("remote:");
}
