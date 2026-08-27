import type { CodeRepoSnapshot, CodeWorkspaceSnapshot } from "../api/types";

/** Pretty JSON that still fits a first turn. Events are the usual overflow. */
export const DEBUG_JSON_PROMPT_BUDGET = 100_000;

const PRODUCT_REPO_NAMES = new Set(["tidebreak", "brightwave-inc/tidebreak"]);

export function repoPathBasename(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] ?? "";
}

/**
 * The Tidebreak source checkout among connected repos.
 *
 * Match the folder or display name, not a worktree path: those live under
 * `workspaces/tidebreak/…` and are not the product repository.
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

export function uneffMePrompt(input: {
  sourceTitle: string;
  sourceBranch: string;
  sourceRepo: string;
  sessionId: string;
  debug: unknown;
}): string {
  const json = debugJsonForPrompt(input.debug);
  const omission =
    json.omitted === "events"
      ? "Journal events were omitted because the debug dump was too large. Session and turns are included."
      : json.omitted === "truncated"
        ? "The debug dump was truncated because it was too large."
        : null;
  return [
    "The user hit a problem in Tidebreak Code. You are in a fresh workspace of the Tidebreak source repository.",
    "Diagnose the product bug from the debug JSON. Fix it. Open a pull request against main when the fix is ready.",
    `Source workspace: ${input.sourceTitle}`,
    `Source branch: ${input.sourceBranch}`,
    `Source repository: ${input.sourceRepo}`,
    `Session: ${input.sessionId}`,
    omission,
    "The debug JSON follows.",
    json.text,
  ]
    .filter((line): line is string => Boolean(line))
    .join("\n\n");
}

export async function startUneffMeWorkspace(input: {
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
}): Promise<{ workspace: CodeWorkspaceSnapshot; prompt: string }> {
  const repo = tidebreakProductRepo(input.repos);
  if (!repo) {
    throw new Error("Add the Tidebreak repository to Code first.");
  }
  const debug = await input.getDebug(input.sessionId);
  const prompt = uneffMePrompt({
    sourceTitle: input.sourceTitle,
    sourceBranch: input.sourceBranch,
    sourceRepo: input.sourceRepo,
    sessionId: input.sessionId,
    debug,
  });
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
