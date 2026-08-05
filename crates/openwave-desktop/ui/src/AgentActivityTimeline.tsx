import { useEffect, useId, useState } from "react";
import { ChevronRight } from "lucide-react";

import type { AgentActivityHistoryEntry } from "./api";
import { agentActivityHistoryLabel } from "./AgentRunDisplay";
import { ToolIcon } from "./ToolIcon";
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
}: {
  state: AgentActivityState;
  active: boolean;
}) {
  const contentId = useId();
  const [expanded, setExpanded] = useState(active);
  const [userToggled, setUserToggled] = useState(false);

  useEffect(() => {
    if (!userToggled) setExpanded(active);
  }, [active, userToggled]);

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

  return (
    <div className="flex flex-col gap-0.5">
      <Button
        type="button"
        variant="link"
        className="h-auto w-fit gap-1.5 px-0 py-0.5 text-xs text-muted-foreground"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => {
          setUserToggled(true);
          setExpanded((current) => !current);
        }}
      >
        <ChevronRight
          className={cn("size-3 transition-transform", expanded && "rotate-90")}
          aria-hidden="true"
        />
        {agentActivitySummaryLabel(summary)}
      </Button>
      {expanded && (
        <ol
          id={contentId}
          className="relative ml-1.5 flex flex-col gap-3 border-l-2 border-border py-1 pl-4 text-sm"
          role="list"
        >
          {state.items.map((entry, index) => {
            const tone = activityOutcomeTone(entry.outcome);
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
                    "min-w-0 flex-1 truncate font-medium text-foreground",
                    tone === "running" && "animate-pulse",
                  )}
                >
                  {agentActivityHistoryLabel(entry)}
                </span>
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
