import { useEffect, useState } from "react";

import type { AgentActivityHistoryEntry } from "./api";
import {
  agentActivityHistoryLabel,
  getAgentActivityOutcomeDotClass,
} from "./AgentRunDisplay";
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

/** The ordered timeline itself: one dot, phrase, and time per recorded step. */
export function AgentActivityTimeline({ state }: { state: AgentActivityState }) {
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
  return (
    <ol className="flex flex-col gap-2 border-l-2 border-border py-0.5 pl-3" role="list">
      {state.items.map((entry, index) => (
        <li key={`${entry.at}:${index}`} className="flex items-center gap-2 text-xs">
          <span
            className={cn(
              "size-1.5 shrink-0 rounded-full",
              getAgentActivityOutcomeDotClass(entry.outcome),
            )}
            aria-hidden="true"
          />
          <span className="min-w-0 flex-1 truncate text-foreground">
            {agentActivityHistoryLabel(entry)}
          </span>
          <time className="shrink-0 text-muted-foreground" dateTime={entry.at}>
            {formatActivityTime(entry.at)}
          </time>
        </li>
      ))}
    </ol>
  );
}

function formatActivityTime(at: string): string {
  const parsed = new Date(at);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
