import type {
  AgentActivityHistoryEntry,
  AgentRun,
  AgentRunProgressEntry,
} from "./api";
import { agentActivityHistoryLabel } from "./AgentRunDisplay";
import { friendlyErrorMessage } from "./lib/utils";

/**
 * "Copy debug info" for one background agent run — the per-run counterpart of
 * the chat-level debug bundle in ChatDebugBundle.ts. There the document is
 * built natively from the journal; a background run has no journal surface of
 * its own, so this report is assembled client-side from the same
 * renderer-safe endpoints the panel already reads: the run snapshot, the
 * ordered activity history, and the run's published progress lines.
 *
 * The formatter is a pure function so the document is testable without the
 * client; fetching and degrading are the orchestration below it.
 */

/** Everything the report needs, already fetched (or already failed). */
export type AgentRunDebugReportInput = {
  run: AgentRun;
  /** Null when the history fetch failed; the section says so instead. */
  activity: AgentActivityHistoryEntry[] | null;
  /** Null when the progress fetch failed; the section says so instead. */
  progress: AgentRunProgressEntry[] | null;
};

function valueOrDash(value: string | number | null | undefined): string {
  if (value === null || value === undefined || value === "") return "—";
  return String(value);
}

/** ISO, verbatim — a debug report wants the exact timestamp, not a locale's. */
function timestamp(at: string | null | undefined): string {
  return valueOrDash(at);
}

function formatActivityEntry(
  entry: AgentActivityHistoryEntry,
  index: number,
): string[] {
  const label = agentActivityHistoryLabel(entry);
  const lines = [
    `${index + 1}. **${label}** — ${entry.outcome} · ${timestamp(entry.at)}`,
  ];
  const detail = entry.detail;
  if (!detail) return lines;
  switch (detail.kind) {
    case "exec": {
      const command = [detail.command, ...detail.args].join(" ");
      lines.push(`   - Command: \`${command}\``);
      if (detail.exit_code !== undefined) {
        lines.push(`   - Exit code: ${detail.exit_code}`);
      }
      if (detail.output) {
        lines.push("   - Output:", "", "```", detail.output, "```", "");
      }
      return lines;
    }
    case "search":
      lines.push(`   - Query: ${detail.query}`);
      return lines;
    case "file":
      lines.push(`   - File: ${detail.name}`);
      return lines;
  }
}

/**
 * The Markdown document for one run. Sections are stable so a report can be
 * compared across copies: summary fields first, then the activity timeline in
 * its recorded order, then the run's published progress, then its result.
 */
export function formatAgentRunDebugReport({
  run,
  activity,
  progress,
}: AgentRunDebugReportInput): string {
  const parts: string[] = [
    "# Background agent run debug info",
    "",
    "## Run",
    "",
    `- Run ID: ${run.id}`,
    `- Parent run ID: ${valueOrDash(run.parent_id)}`,
    `- Spawn call ID: ${valueOrDash(run.spawn_call_id)}`,
    `- Tier: ${run.tier}`,
    `- Execution location: ${run.execution_location}`,
    `- Status: ${run.status}`,
    `- Last error code: ${valueOrDash(run.last_error_code)}`,
    `- Task: ${valueOrDash(run.task)}`,
    `- Created: ${timestamp(run.created_at)}`,
    `- Updated: ${timestamp(run.updated_at)}`,
    `- Started: ${timestamp(run.started_at)}`,
    `- Finished: ${timestamp(run.finished_at)}`,
    "",
  ];

  parts.push("## Activity", "");
  if (activity === null) {
    parts.push("_Activity history could not be fetched._", "");
  } else if (activity.length === 0) {
    parts.push("_No recorded activity._", "");
  } else {
    activity.forEach((entry, index) => {
      parts.push(...formatActivityEntry(entry, index));
    });
    parts.push("");
  }

  parts.push("## Progress", "");
  if (progress === null) {
    parts.push("_Progress lines could not be fetched._", "");
  } else if (progress.length === 0) {
    parts.push("_No progress published._", "");
  } else {
    for (const line of progress) {
      parts.push(`- ${timestamp(line.at)} — ${line.text}`);
    }
    parts.push("");
  }

  if (run.terminal_text) {
    parts.push("## Result", "", run.terminal_text, "");
  }

  return parts.join("\n").trimEnd() + "\n";
}

export type AgentRunDebugDeps = {
  fetchActivity: () => Promise<AgentActivityHistoryEntry[]>;
  fetchProgress: () => Promise<AgentRunProgressEntry[]>;
  writeClipboard: (text: string) => Promise<void>;
  notify: (notice: { message: string; description?: string }) => void;
};

/** Read every progress page, resuming from each page's cursor. */
export async function fetchAgentRunProgress(
  listPage: (
    afterSequence: number,
  ) => Promise<{ entries: AgentRunProgressEntry[]; nextSequence: number }>,
): Promise<AgentRunProgressEntry[]> {
  const entries: AgentRunProgressEntry[] = [];
  let afterSequence = 0;
  // Pages resume strictly forward, so once a page arrives empty there is no
  // more to read — the cursor only matters while entries keep arriving.
  for (;;) {
    const page = await listPage(afterSequence);
    entries.push(...page.entries);
    if (page.entries.length === 0 || page.nextSequence <= afterSequence) {
      return entries;
    }
    afterSequence = page.nextSequence;
  }
}

/**
 * Build and copy the report for one run. Activity and progress failures each
 * degrade to a note inside their section — the snapshot alone is still worth
 * attaching — while a total failure (the clipboard write, or the run itself
 * being unreadable) surfaces the way errors do elsewhere: one sonner toast.
 */
export async function copyAgentRunDebug(
  run: AgentRun,
  deps: AgentRunDebugDeps,
): Promise<void> {
  try {
    const [activity, progress] = await Promise.all([
      deps.fetchActivity().catch(() => null),
      deps.fetchProgress().catch(() => null),
    ]);
    await deps.writeClipboard(formatAgentRunDebugReport({ run, activity, progress }));
    deps.notify({
      message: "Debug info copied",
      description:
        "Includes the run's task, activity history, and progress. Review it before sharing.",
    });
  } catch (caught) {
    deps.notify({
      message: friendlyErrorMessage(caught, "Could not copy debug info."),
    });
  }
}
