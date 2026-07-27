import { useState } from "react";
import { Bot, ChevronRight } from "lucide-react";

import type { AgentRun } from "./api";
import {
  AGENT_RUN_STATUS_GROUPS,
  agentRunStatusDetail,
  getAgentRunDotClass,
} from "./AgentRunDisplay";
import type { ToolCallStatus } from "./ToolCallCard";
import { cn } from "@/lib/utils";

export type BackgroundAgentSpawn = {
  callId: string;
  runId?: string;
  status: ToolCallStatus;
};

type BackgroundAgentListProps = {
  spawns: readonly BackgroundAgentSpawn[];
  runs: readonly AgentRun[];
  loading: boolean;
  error: string | null;
  onRetry: () => void;
};

/**
 * One transcript-local view of the agents requested by a single spawn step.
 * It observes chat-scoped snapshots only; no scheduler or worker state can be
 * changed from this card.
 */
export function BackgroundAgentList({
  spawns,
  runs,
  loading,
  error,
  onRetry,
}: BackgroundAgentListProps) {
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const sandboxRuns = runs.filter((run) => run.execution === "sandbox");
  const matchedRuns = sandboxRuns.filter((run) =>
    spawns.some(
      (spawn) =>
        (spawn.runId !== undefined && run.id === spawn.runId) ||
        run.spawn_call_id === spawn.callId,
    ),
  );
  const matchedRunIds = new Set(matchedRuns.map((run) => run.id));
  const visibleSpawns = spawns.filter(
    (spawn) =>
      matchedRunIds.has(spawn.runId ?? "") ||
      matchedRuns.some((run) => run.spawn_call_id === spawn.callId) ||
      (spawn.status !== "failed" && spawn.status !== "cancelled"),
  );
  const unresolvedSpawns = visibleSpawns.filter(
    (spawn) =>
      !matchedRunIds.has(spawn.runId ?? "") &&
      !matchedRuns.some((run) => run.spawn_call_id === spawn.callId),
  );

  if (matchedRuns.length === 0 && unresolvedSpawns.length === 0 && !error) {
    return null;
  }

  const toggleGroup = (id: string) => {
    setCollapsedGroups((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <section
      className="mt-2 overflow-hidden rounded-lg border border-border bg-background"
      aria-label="Background agents"
    >
      <div className="flex items-center gap-2 px-3 py-2 text-sm font-medium text-muted-foreground">
        <Bot className="size-4" aria-hidden="true" />
        <span>
          {visibleSpawns.length === 1
            ? "1 background agent"
            : `${visibleSpawns.length} background agents`}
        </span>
      </div>
      {error ? (
        <div className="flex items-center justify-between gap-3 border-t border-border px-3 py-2 text-sm" role="status">
          <span>Background agent status is unavailable.</span>
          <button
            type="button"
            className="shrink-0 font-medium text-primary hover:underline"
            onClick={onRetry}
          >
            Retry
          </button>
        </div>
      ) : (
        <div aria-live="polite">
          {AGENT_RUN_STATUS_GROUPS.map((group) => {
            const groupRuns = matchedRuns.filter((run) => group.statuses.includes(run.status));
            if (groupRuns.length === 0) return null;
            const collapsed = collapsedGroups.has(group.id);
            return (
              <div key={group.id}>
                <button
                  type="button"
                  className="flex w-full items-center gap-1.5 border-y border-border bg-muted/50 px-3 py-1.5 text-left text-xs font-medium text-foreground hover:bg-muted/60"
                  onClick={() => toggleGroup(group.id)}
                  aria-expanded={!collapsed}
                >
                  <ChevronRight className={cn("size-3 shrink-0 transition-transform", !collapsed && "rotate-90")} />
                  <span
                    className={cn("size-2 shrink-0 rounded-full", getAgentRunDotClass(group.statuses[0]!))}
                    aria-hidden="true"
                  />
                  <span>{group.label}</span>
                  <span className="text-muted-foreground">{groupRuns.length}</span>
                </button>
                {!collapsed && (
                  <div className="divide-y divide-border/60">
                    {groupRuns.map((run, index) => (
                      <div key={run.id} className="flex min-w-0 items-center gap-2 px-3 py-2.5">
                        <span
                          className={cn("size-2 shrink-0 rounded-full", getAgentRunDotClass(run.status))}
                          aria-hidden="true"
                        />
                        <span className="min-w-0 flex-1 truncate font-medium text-foreground">
                          Background agent {index + 1}
                        </span>
                        <span className="shrink-0 text-xs text-muted-foreground">
                          {agentRunStatusDetail(run)}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
          {unresolvedSpawns.map((spawn) => (
            <div key={spawn.callId} className="flex min-w-0 items-center gap-2 border-t border-border px-3 py-2.5">
              <span className="size-2 shrink-0 animate-pulse rounded-full bg-muted-foreground" aria-hidden="true" />
              <span className="h-4 w-28 animate-pulse rounded bg-muted" aria-hidden="true" />
              <span className="text-sm text-muted-foreground">
                {loading ? "Starting background agent" : "Waiting for background agent"}
              </span>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
