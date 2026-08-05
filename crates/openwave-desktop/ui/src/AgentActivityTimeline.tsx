import { useEffect, useId, useState } from "react";
import { ChevronRight } from "lucide-react";

import type { AgentActivityHistoryEntry } from "./api";
import { agentActivityHistoryLabel } from "./AgentRunDisplay";
import { ToolIcon } from "./ToolIcon";
import { execCommandHeadline } from "./ToolPreview";
import { ToolStatusIcon, type ToolTone } from "./ToolStatusIcon";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export type AgentActivityState = {
  loading: boolean;
  error: boolean;
  loaded: boolean;
  items: AgentActivityHistoryEntry[];
};

/**
 * A background run's ordered activity history, re-read whenever the observed
 * run advances (`updatedAt` changes) so a live run's steps settle in place
 * without a manual refresh. `enabled` gates the fetch entirely — a collapsed
 * row or an absent run reads nothing.
 */
export function useAgentRunActivity(
  runId: string | null,
  updatedAt: string | undefined,
  enabled: boolean,
  loadActivity: (runId: string) => Promise<AgentActivityHistoryEntry[]>,
): AgentActivityState {
  const [activity, setActivity] = useState<AgentActivityState>({
    loading: false,
    error: false,
    loaded: false,
    items: [],
  });

  useEffect(() => {
    if (!enabled || runId === null) return;
    let cancelled = false;
    setActivity((state) => ({ ...state, loading: !state.loaded, error: false }));
    loadActivity(runId)
      .then((items) => {
        if (!cancelled) {
          setActivity({ loading: false, error: false, loaded: true, items });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setActivity((state) => ({ ...state, loading: false, error: true }));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [enabled, runId, updatedAt, loadActivity]);

  return activity;
}

export type AgentActivitySummary = {
  toolCalls: number;
  failed: number;
};

export function summarizeAgentActivity(
  items: readonly AgentActivityHistoryEntry[],
): AgentActivitySummary {
  return {
    toolCalls: items.length,
    failed: items.filter((entry) => entry.outcome === "failed").length,
  };
}

/**
 * The ordered timeline itself: the same collapsible rail foreground tool calls
 * use, reduced to the renderer-safe label, outcome, and time the history owns.
 */
export function AgentActivityTimeline({
  state,
  active,
  activeLabel,
  expanded: controlledExpanded,
  onExpandedChange,
}: {
  state: AgentActivityState;
  active: boolean;
  activeLabel?: string;
  expanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
}) {
  const contentId = useId();
  const [expandedPreference, setExpandedPreference] = useState<boolean | null>(
    null,
  );
  const expanded = controlledExpanded ?? expandedPreference ?? active;

  if (state.error) {
    return (
      <p className="text-xs text-muted-foreground" role="status">
        Activity history is unavailable.
      </p>
    );
  }
  if (state.items.length === 0) {
    return (
      <p className="text-xs text-muted-foreground" role="status">
        {state.loading && !state.loaded
          ? "Loading activity…"
          : "No recorded activity yet."}
      </p>
    );
  }

  const summary = summarizeAgentActivity(state.items);
  const latest = state.items[state.items.length - 1]!;
  const triggerLabel = active
    ? (activeLabel ?? agentActivityHistoryLabel(latest))
    : agentActivitySummaryLabel(summary);

  return (
    <div className="flex flex-col gap-0.5">
      <Button
        type="button"
        variant="link"
        className="h-auto w-fit gap-1.5 px-0 py-0.5 text-xs text-muted-foreground"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => {
          const next = !expanded;
          if (onExpandedChange) onExpandedChange(next);
          else setExpandedPreference(next);
        }}
      >
        <ChevronRight
          className={cn("size-3 transition-transform", expanded && "rotate-90")}
          aria-hidden="true"
        />
        <span>{triggerLabel}</span>
      </Button>
      {expanded && (
        <ol
          id={contentId}
          className="relative ml-1.5 flex flex-col gap-3 border-l-2 border-border py-1 pl-4 text-sm"
          role="list"
        >
          {state.items.map((entry, index) => {
            const tone = activityOutcomeTone(entry.outcome);
            const headline = activityHeadline(entry.detail);
            const exitCode = failedExitCode(entry);
            return (
              <li
                key={`${entry.at}:${index}`}
                className="flex min-w-0 items-center gap-1.5"
              >
                <div
                  className="absolute -left-px -translate-x-1/2 bg-background py-0.5 text-muted-foreground [&_svg]:size-4"
                  aria-hidden="true"
                >
                  <ToolIcon name={entry.kind} />
                </div>
                <ToolStatusIcon
                  tone={tone}
                  className={cn(
                    "size-4 shrink-0",
                    tone === "failed" && "text-critical",
                  )}
                />
                <span
                  className={cn(
                    "min-w-0 truncate font-medium text-foreground",
                    headline === null ? "flex-1" : "shrink-0",
                    tone === "running" && "animate-pulse",
                  )}
                >
                  {agentActivityHistoryLabel(entry)}
                </span>
                {headline !== null && (
                  <span
                    className={cn(
                      "min-w-0 flex-1 truncate text-xs text-muted-foreground",
                      headline.monospace && "font-mono",
                    )}
                  >
                    {headline.text}
                  </span>
                )}
                {exitCode !== null && (
                  <span className="shrink-0 text-xs font-medium text-destructive">
                    Exit {exitCode}
                  </span>
                )}
                <time
                  className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground"
                  dateTime={entry.at}
                >
                  {formatActivityTime(entry.at)}
                </time>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}

/**
 * The step's own headline, next to the generic label.
 *
 * "Ran a command" is the same sentence whichever command ran, so the validated
 * detail is what tells the steps apart. It stays a truncated single line: this
 * row is all the renderer has, because no output is retained for a background
 * run's calls.
 */
function activityHeadline(
  detail: AgentActivityHistoryEntry["detail"],
): { text: string; monospace: boolean } | null {
  if (detail === undefined) return null;
  switch (detail.kind) {
    case "exec":
      return {
        text: execCommandHeadline(detail.command, detail.args),
        monospace: true,
      };
    case "search":
      return { text: detail.query, monospace: false };
    case "file":
      return { text: detail.name, monospace: false };
  }
}

/**
 * The exit status of a command that failed, when the receipt recorded one. A
 * non-zero exit is the most specific thing the timeline can say about a failed
 * step, the same way the foreground badge prefers it over "failed".
 */
function failedExitCode(entry: AgentActivityHistoryEntry): number | null {
  if (entry.outcome !== "failed") return null;
  const detail = entry.detail;
  if (detail?.kind !== "exec" || detail.exit_code === undefined) return null;
  return detail.exit_code;
}

function agentActivitySummaryLabel(summary: AgentActivitySummary): string {
  const toolCalls = `${summary.toolCalls} tool ${summary.toolCalls === 1 ? "call" : "calls"}`;
  const failures = summary.failed > 0 ? ` · ${summary.failed} failed` : "";
  return `Ran ${toolCalls}${failures}`;
}

function activityOutcomeTone(
  outcome: AgentActivityHistoryEntry["outcome"],
): ToolTone {
  switch (outcome) {
    case "waiting":
      return "waiting_approval";
    case "running":
      return "running";
    case "completed":
      return "completed";
    case "failed":
      return "failed";
    case "cancelled":
      return "cancelled";
  }
}

function formatActivityTime(at: string): string {
  const parsed = new Date(at);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
