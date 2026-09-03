import type {
  CodeRepoSnapshot,
  CodeWorkspaceSnapshot,
  HarnessDoctorReport,
  HarnessKind,
  PermissionMode,
} from "../api/types";
import { messageWithPastedText } from "../PastedText";
import { clampPermissionMode } from "../PermissionModeMenu";
import type { CodeCreateDefaults, WorkspaceStartupStep } from "./CodeUiStore";
import {
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessCanStartNow,
  workspaceHarnesses,
} from "./labels";
import type { FirstSessionSettings } from "./startWorkspaceSession";

/** Pretty JSON that still fits a first turn. Events are the usual overflow. */
export const DEBUG_JSON_PROMPT_BUDGET = 100_000;

/** Where issues and pull requests go. */
export const TIDEBREAK_GITHUB_REPO = "brightwave-inc/tidebreak";

/** The heading the startup handoff shows while Uneff me gets going. */
export const UNEFF_STARTUP_HEADING = "Getting Tidebreak ready to help";

const PRODUCT_REPO_NAMES = new Set(["tidebreak", TIDEBREAK_GITHUB_REPO]);

export function repoPathBasename(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] ?? "";
}

/**
 * The Tidebreak source checkout among connected repos.
 *
 * Match the folder or display name, not a worktree path: those live under
 * `workspaces/tidebreak/…` and are not the product repository. Record 81:
 * a connected checkout gets a fix workspace; without one, Uneff me runs in
 * the session's own workspace and never clones on the reader's behalf.
 */
export function isTidebreakProductRepo(repo: {
  display_name: string;
  root_path: string;
}): boolean {
  const name = repo.display_name.trim().toLowerCase();
  if (PRODUCT_REPO_NAMES.has(name)) return true;
  return repoPathBasename(repo.root_path).toLowerCase() === "tidebreak";
}

export function tidebreakProductRepo<
  T extends { display_name: string; root_path: string },
>(repos: readonly T[]): T | undefined {
  const matches = repos.filter(isTidebreakProductRepo);
  return (
    matches.find(
      (repo) => repo.display_name.trim().toLowerCase() === "tidebreak",
    ) ??
    matches.find(
      (repo) => repoPathBasename(repo.root_path).toLowerCase() === "tidebreak",
    ) ??
    matches[0]
  );
}

export function uneffMeWorkspaceTitle(sourceTitle: string): string {
  const base = `Uneff: ${sourceTitle.trim() || "session"}`;
  return base.length > 60 ? `${base.slice(0, 59).trimEnd()}…` : base;
}

export type DebugJsonOmission = "none" | "events" | "truncated";

export function debugJsonForPrompt(debug: unknown): {
  text: string;
  omitted: DebugJsonOmission;
} {
  const pretty = stringifyDebug(debug, true);
  if (pretty.length <= DEBUG_JSON_PROMPT_BUDGET) {
    return { text: pretty, omitted: "none" };
  }
  const record = asRecord(debug);
  if (record && "events" in record) {
    const { events: _events, ...rest } = record;
    const withoutEventsPretty = stringifyDebug(rest, true);
    if (withoutEventsPretty.length <= DEBUG_JSON_PROMPT_BUDGET) {
      return { text: withoutEventsPretty, omitted: "events" };
    }
    const withoutEventsCompact = stringifyDebug(rest, false);
    if (withoutEventsCompact.length <= DEBUG_JSON_PROMPT_BUDGET) {
      return { text: withoutEventsCompact, omitted: "events" };
    }
    return {
      text: truncateDebug(withoutEventsCompact),
      omitted: "truncated",
    };
  }
  const compact = stringifyDebug(debug, false);
  if (compact.length <= DEBUG_JSON_PROMPT_BUDGET) {
    return { text: compact, omitted: "none" };
  }
  return { text: truncateDebug(compact), omitted: "truncated" };
}

/**
 * The first turn of an Uneff me session.
 *
 * The agent asks before it acts: what went wrong, and whether the user wants
 * an issue or a pull request. Both land on the public Tidebreak repository,
 * so the prompt also says what must not be pasted there. The debug report
 * rides along the way a long clipboard paste does, so the transcript folds it
 * instead of printing a hundred kilobytes of JSON.
 */
export function uneffMePrompt(input: {
  sourceTitle: string;
  sourceBranch: string;
  sourceRepo: string;
  sessionId: string;
  debug: unknown;
  /** The session runs on a Tidebreak checkout, so a fix can start at once. */
  inTidebreakCheckout: boolean;
}): string {
  const json = debugJsonForPrompt(input.debug);
  const omission =
    json.omitted === "events"
      ? "Journal events were omitted because the debug report was too large. Session and turns are included."
      : json.omitted === "truncated"
        ? "The debug report was truncated because it was too large."
        : null;
  const where = input.inTidebreakCheckout
    ? `You are in a fresh workspace on the Tidebreak source repository (${TIDEBREAK_GITHUB_REPO}). The repository is public.`
    : `You are in the user's own workspace, not a Tidebreak checkout. Tidebreak's source is the public repository ${TIDEBREAK_GITHUB_REPO}.`;
  const pullRequest = input.inTidebreakCheckout
    ? "To open a pull request, diagnose the bug from the debug report, fix it, add a test only if a failure would change what we do, and open the pull request against main. If you cannot push to origin, fork with `gh repo fork --remote` and open the pull request from the fork."
    : `To open a pull request, first ask the user where you may clone Tidebreak, and clone it only after they say yes: \`gh repo fork ${TIDEBREAK_GITHUB_REPO} --clone\` into that folder, or \`gh repo clone ${TIDEBREAK_GITHUB_REPO}\` if they have push access. Do not clone anything without asking. Then diagnose the bug from the debug report, fix it, add a test only if a failure would change what we do, and open the pull request against main. Mention that adding the Tidebreak repository to Code makes the next Uneff me start in a Tidebreak workspace directly.`;
  const draft = [
    `The user hit a problem in Tidebreak Code and asked for help. ${where}`,
    "Start by asking the user what went wrong and what they want: a GitHub issue that describes the problem, or a pull request that fixes it. Keep the questions short and do not investigate or change anything until they answer. The debug report below already carries the session, its turns, and the journal events, so ask only for what it cannot tell you.",
    `To file an issue, run \`gh issue create --repo ${TIDEBREAK_GITHUB_REPO}\` with a clear title, the steps, what happened, what was expected, and the parts of the debug report that show it. Never paste the whole report. Strip file paths, tokens, prompts, and anything else the user would not want public, and show the user the issue text before you file it.`,
    pullRequest,
    "If `gh` is missing or not signed in, tell the user how to fix that (`gh auth login`) before you file anything.",
    `Source workspace: ${input.sourceTitle}`,
    `Source branch: ${input.sourceBranch}`,
    `Source repository: ${input.sourceRepo}`,
    `Session: ${input.sessionId}`,
    omission,
    "The debug report follows as pasted text.",
  ]
    .filter((line): line is string => Boolean(line))
    .join("\n\n");
  return messageWithPastedText(draft, [
    { id: "uneff-debug-report", text: json.text },
  ]);
}

/** What Uneff me is doing before the session starts. */
export type UneffProgress = { step: "debug" } | { step: "create" };

/** The handoff's preparation steps for one point in the Uneff me flow. */
export function uneffPreparationSteps(
  progress: UneffProgress,
): WorkspaceStartupStep[] {
  const debug = "Collecting the debug report";
  return [
    { label: debug, state: progress.step === "debug" ? "active" : "complete" },
  ];
}

/**
 * The engine and posture an Uneff me session starts with.
 *
 * There is no dialog to pick in, so the choice follows what the reader was
 * just using: the source session's engine when it can start, else the last
 * create's, else any engine that can. Null means nothing can start, or the
 * managed policy forbids every posture the engine honors.
 */
export function uneffSessionSettings(input: {
  doctor: HarnessDoctorReport | null;
  sourceHarness?: HarnessKind;
  lastCreate: CodeCreateDefaults | null;
  ceiling: PermissionMode | null | undefined;
}): FirstSessionSettings | null {
  const ready = workspaceHarnesses(input.doctor?.harnesses ?? []).filter(
    harnessCanStartNow,
  );
  const entry =
    ready.find((candidate) => candidate.kind === input.sourceHarness) ??
    ready.find((candidate) => candidate.kind === input.lastCreate?.harness) ??
    ready[0];
  if (!entry) return null;
  const available = createPermissionModes(entry.caps);
  const remembered = input.lastCreate?.permissionMode;
  const requested =
    remembered && available.includes(remembered)
      ? remembered
      : defaultCreatePermissionMode(entry.caps);
  const permissionMode = clampPermissionMode(
    requested,
    input.ceiling,
    available,
  );
  if (!permissionMode) return null;
  const harness = entry.kind;
  return {
    harness,
    permissionMode,
    model: input.lastCreate?.modelsByHarness[harness],
    reasoningEffort: input.lastCreate?.reasoningEffortByHarness?.[harness],
    fastMode: input.lastCreate?.fastModeByHarness?.[harness],
  };
}

/**
 * Collect the debug report and, when a Tidebreak checkout is connected,
 * create the workspace the fix session runs in.
 *
 * With no checkout the session runs where the reader already is: `workspace`
 * comes back null and the prompt says so. Nothing is cloned for them — an
 * issue needs no source tree, and a pull request is the agent's to ask about.
 * `onProgress` fires before each step so the caller can draw it.
 */
export async function prepareUneffMe(input: {
  repos: readonly CodeRepoSnapshot[];
  sessionId: string;
  sourceTitle: string;
  sourceBranch: string;
  sourceRepo: string;
  getDebug: (sessionId: string) => Promise<unknown>;
  createWorkspace: (body: {
    repo_id: string;
    title?: string;
  }) => Promise<CodeWorkspaceSnapshot>;
  onProgress?: (progress: UneffProgress) => void;
}): Promise<{ workspace: CodeWorkspaceSnapshot | null; prompt: string }> {
  const repo = tidebreakProductRepo(input.repos);
  input.onProgress?.({ step: "debug" });
  const debug = await input.getDebug(input.sessionId);
  const prompt = uneffMePrompt({
    sourceTitle: input.sourceTitle,
    sourceBranch: input.sourceBranch,
    sourceRepo: input.sourceRepo,
    sessionId: input.sessionId,
    debug,
    inTidebreakCheckout: Boolean(repo),
  });
  if (!repo) return { workspace: null, prompt };
  input.onProgress?.({ step: "create" });
  const workspace = await input.createWorkspace({
    repo_id: repo.id,
    title: uneffMeWorkspaceTitle(input.sourceTitle),
  });
  return { workspace, prompt };
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function stringifyDebug(value: unknown, pretty: boolean): string {
  return pretty ? JSON.stringify(value, null, 2) : JSON.stringify(value);
}

function truncateDebug(text: string): string {
  return `${text.slice(0, DEBUG_JSON_PROMPT_BUDGET)}\n…(truncated)`;
}
