import { useEffect, useState } from "react";
import { Bot, ChevronDown, ChevronRight, Square } from "lucide-react";

import type { AgentActivityHistoryEntry, AgentRun } from "./api";
import { AgentActivityTimeline, useAgentRunActivity } from "./AgentActivityTimeline";
import {
  AGENT_RUN_STATUS_GROUPS,
  agentRunStatusDetail,
  getAgentRunDotClass,
  RUNNING_AGENT_STATUSES,
} from "./AgentRunDisplay";
import { SubmittedOutputPills } from "./SubmittedOutputPills";
import type { ToolCallStatus } from "./ToolCallCard";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
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
  /** Durably request cancellation of one background run. */
  onCancel: (runId: string) => Promise<void>;
  /** Fetch a run's ordered, renderer-safe activity history. */
  onLoadActivity: (runId: string) => Promise<AgentActivityHistoryEntry[]>;
  /** Open one run's dedicated panel beside the conversation. */
  onOpen?: (runId: string) => void;
  /** Open one file a run submitted, from its pill on the row. */
  onOpenOutput?: (outputId: string) => void;
};

/**
 * One transcript-local view of the agents requested by a single spawn step.
 * It observes chat-scoped snapshots only; the one command it can issue is a
 * durable cancellation request, resolved by the same read model it polls.
 */
export function BackgroundAgentList({
  spawns,
  runs,
  loading,
  error,
  onRetry,
  onCancel,
  onLoadActivity,
  onOpen,
  onOpenOutput,
}: BackgroundAgentListProps) {
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const sandboxRuns = runs.filter((run) => run.tier === "background");
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
        <div className="max-h-48 overflow-y-auto" aria-live="polite">
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
                    {groupRuns.map((run) => (
                      <BackgroundAgentRow
                        key={run.id}
                        run={run}
                        onCancel={onCancel}
                        onLoadActivity={onLoadActivity}
                        onOpen={onOpen}
                        onOpenOutput={onOpenOutput}
                      />
                    ))}
                  </div>
                )}
              </div>
            );
          })}
          {unresolvedSpawns.map((spawn) => (
            <div key={spawn.callId} className="flex min-w-0 items-center gap-2 border-t border-border px-3 py-2.5">
              <Skeleton className="size-2 shrink-0 rounded-full bg-muted-foreground" aria-hidden="true" />
              <Skeleton className="h-4 w-28" aria-hidden="true" />
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

/**
 * One background run: the task it was given, its live status, and a Stop control
 * while it is cancellable, expandable into the ordered timeline of what it has
 * done. The row itself opens the run's dedicated panel, which is where the
 * result belongs — a finished agent's full text inline would bury every sibling
 * row under it, and a delegation of any size is a list to scan, not to read.
 */
function BackgroundAgentRow({
  run,
  onCancel,
  onLoadActivity,
  onOpen,
  onOpenOutput,
}: {
  run: AgentRun;
  onCancel: (runId: string) => Promise<void>;
  onLoadActivity: (runId: string) => Promise<AgentActivityHistoryEntry[]>;
  onOpen?: (runId: string) => void;
  onOpenOutput?: (outputId: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [stopping, setStopping] = useState(false);
  const activity = useAgentRunActivity(
    run.id,
    run.updated_at,
    expanded,
    onLoadActivity,
  );

  // The Stop control is offered only while a fresh cancel would still change
  // anything. A run that is already `cancelling` shows a settled "Stopping".
  const stoppable =
    RUNNING_AGENT_STATUSES.has(run.status) && run.status !== "cancelling";
  const showStopping = stopping || run.status === "cancelling";

  // Drop the optimistic bridge once the durable transition has caught up: any
  // status that is no longer a cancellable running state is confirmation
  // enough (`cancelling`, `cancelled`, or a terminal outcome).
  useEffect(() => {
    if (!stoppable) setStopping(false);
  }, [stoppable]);

  const handleStop = async () => {
    setStopping(true);
    try {
      await onCancel(run.id);
    } catch {
      // The durable request did not commit; return the control so the reader
      // can try again rather than leave a run stuck reading "Stopping".
      setStopping(false);
    }
  };

  const contentId = `background-agent-activity-${run.id}`;

  return (
    <div className="px-3 py-2.5">
      <div className="flex min-w-0 items-center gap-2">
        <button
          type="button"
          className="shrink-0"
          onClick={() => setExpanded((open) => !open)}
          aria-expanded={expanded}
          aria-controls={contentId}
          aria-label={expanded ? "Hide activity" : "Show activity"}
        >
          <ChevronDown
            className={cn(
              "size-3.5 shrink-0 text-muted-foreground transition-transform",
              !expanded && "-rotate-90",
            )}
            aria-hidden="true"
          />
        </button>
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
          onClick={() =>
            onOpen ? onOpen(run.id) : setExpanded((open) => !open)
          }
        >
          <span
            className={cn("size-2 shrink-0 rounded-full", getAgentRunDotClass(run.status))}
            aria-hidden="true"
          />
          <span className="min-w-0 flex-1 truncate text-sm text-foreground">
            {run.task ?? "Background agent"}
          </span>
        </button>
        <span className="shrink-0 text-xs text-muted-foreground">
          {showStopping ? "Stopping" : agentRunStatusDetail(run)}
        </span>
        {stoppable && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-7 shrink-0 gap-1 px-2 text-xs"
            onClick={handleStop}
            disabled={stopping}
          >
            <Square className="size-3 fill-current" aria-hidden="true" />
            Stop
          </Button>
        )}
      </div>
      {run.submitted_outputs.length > 0 && (
        <div className="mt-2 pl-5">
          <SubmittedOutputPills
            outputs={run.submitted_outputs}
            onOpenOutput={onOpenOutput}
          />
        </div>
      )}
      {expanded && (
        <div id={contentId} className="mt-2 pl-5">
          <AgentActivityTimeline
            state={activity}
            active={RUNNING_AGENT_STATUSES.has(run.status)}
          />
        </div>
      )}
    </div>
  );
}
