import { useEffect, useState } from "react";
import { Bot, Check, Clock, Square, X } from "lucide-react";

import { useApp } from "./AppContext";
import {
  AgentActivityTimeline,
  summarizeAgentActivity,
  useAgentRunActivity,
} from "./AgentActivityTimeline";
import { agentRunStatusDetail, RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import type { AgentRun } from "./api";
import { MessageMarkdown } from "./MessageMarkdown";
import { SubmittedOutputPills } from "./SubmittedOutputPills";
import { useAgentRuns } from "./useAgentRuns";
import { PanelBreadcrumb } from "@/components/PanelHeader";
import { PanelFrame } from "@/panel/PanelFrame";
import { usePanelNav } from "@/panel/usePanelNav";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";

/**
 * One background run, opened beside the conversation from its row in the
 * transcript's agent list.
 *
 * The panel reads the same durable, renderer-safe snapshot the inline list
 * polls — status, elapsed time, and the ordered activity history — and its one
 * command is the same durable cancellation request. It keeps observing while
 * the run is live, so the header and timeline settle on their own as the run
 * finishes, fails, or stops.
 */
export function BackgroundAgentPanel({
  chatId,
  runId,
}: {
  chatId: string;
  runId: string;
}) {
  const { client } = useApp();
  const { openPanel } = usePanelNav();
  const agentRuns = useAgentRuns(client, chatId, [runId]);
  const run =
    agentRuns.runs.find(
      (candidate) => candidate.id === runId && candidate.tier === "background",
    ) ?? null;

  const stoppable =
    run !== null &&
    RUNNING_AGENT_STATUSES.has(run.status) &&
    run.status !== "cancelling";
  const [stopping, setStopping] = useState(false);
  useEffect(() => {
    if (!stoppable) setStopping(false);
  }, [stoppable]);

  const activity = useAgentRunActivity(
    runId,
    run?.updated_at,
    run !== null,
    agentRuns.loadActivity,
  );

  const stopButton = stoppable ? (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className="h-7 gap-1 px-2 text-xs"
      disabled={stopping}
      onClick={() => {
        setStopping(true);
        agentRuns.cancel(runId).catch(() => setStopping(false));
      }}
    >
      <Square className="size-3 fill-current" aria-hidden="true" />
      {stopping ? "Stopping" : "Stop"}
    </Button>
  ) : undefined;

  return (
    <PanelFrame
      breadcrumb={<PanelBreadcrumb firstPart="Agent" />}
      headerRightSlot={stopButton}
      showBorder
    >
      {run ? (
        <BackgroundAgentDetail
          run={run}
          activity={activity}
          onOpenOutput={(outputId) => openPanel({ type: "outputs", outputId })}
        />
      ) : agentRuns.error ? (
        <div className="flex items-center justify-between gap-3 p-4 text-sm" role="status">
          <span>Background agent status is unavailable.</span>
          <button
            type="button"
            className="shrink-0 font-medium text-primary hover:underline"
            onClick={agentRuns.refresh}
          >
            Retry
          </button>
        </div>
      ) : agentRuns.loading ? (
        <div className="flex flex-col gap-3 p-4" role="status" aria-label="Loading agent">
          <Skeleton className="h-5 w-48" />
          <Skeleton className="h-4 w-64" />
          <Skeleton className="h-24 w-full" />
        </div>
      ) : (
        <p className="p-4 text-sm text-muted-foreground" role="status">
          This agent run is not part of this chat.
        </p>
      )}
    </PanelFrame>
  );
}

function BackgroundAgentDetail({
  run,
  activity,
  onOpenOutput,
}: {
  run: AgentRun;
  activity: ReturnType<typeof useAgentRunActivity>;
  onOpenOutput?: (outputId: string) => void;
}) {
  const live = RUNNING_AGENT_STATUSES.has(run.status);
  const now = useNowWhile(live);
  const elapsed = elapsedLabel(run, now);
  const activitySummary =
    activity.loaded && activity.items.length > 0
      ? summarizeAgentActivity(activity.items)
      : null;

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="shrink-0 px-4 pt-4">
        <div className="flex items-start gap-2.5">
          <div className="grid size-7 shrink-0 place-items-center rounded-lg border bg-muted text-muted-foreground">
            <Bot className="size-4" aria-hidden="true" />
          </div>
          <div className="min-w-0 flex-1">
            <h2 className="line-clamp-2 text-sm font-semibold leading-5">
              {run.task ?? "Background agent"}
            </h2>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {agentRunStatusDetail(run)}
              {elapsed && <> · {elapsed}</>}
              {activitySummary && (
                <>
                  {" · "}
                  {agentActivityMetaLabel(activitySummary)}
                </>
              )}
            </p>
          </div>
          <div className="mt-0.5 shrink-0">
            <AgentRunStatusBadge status={run.status} />
          </div>
        </div>
        {run.status === "failed" && run.last_error_code && (
          <p className="ml-9 mt-1 font-mono text-xs text-critical">
            {run.last_error_code}
          </p>
        )}
      </div>
      <div className="mt-3 flex shrink-0 flex-col gap-2 border-b px-4 pb-3 empty:hidden">
        {run.submitted_outputs.length > 0 && (
          <SubmittedOutputPills
            outputs={run.submitted_outputs}
            onOpenOutput={onOpenOutput}
          />
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto p-4" aria-live="polite">
        <AgentActivityTimeline key={run.id} state={activity} active={live} />
        {run.terminal_text && (
          <section className="mt-4 border-t pt-3.5">
            <p className="mb-2 text-[11px] font-semibold uppercase tracking-[0.06em] text-muted-foreground">
              Result
            </p>
            <div className="break-words text-foreground [&_.message-markdown]:text-sm">
              <MessageMarkdown>{run.terminal_text}</MessageMarkdown>
            </div>
          </section>
        )}
      </div>
    </div>
  );
}

function AgentRunStatusBadge({ status }: { status: AgentRun["status"] }) {
  switch (status) {
    case "completed":
      return (
        <Badge variant="success" size="sm">
          <Check className="size-3" aria-hidden="true" />
          Complete
        </Badge>
      );
    case "failed":
      return (
        <Badge variant="critical" size="sm">
          <X className="size-3" aria-hidden="true" />
          Failed
        </Badge>
      );
    case "cancelled":
      return (
        <Badge variant="critical" size="sm">
          <X className="size-3" aria-hidden="true" />
          Stopped
        </Badge>
      );
    case "cancelling":
      return (
        <Badge variant="outline" size="sm">
          <Spinner className="size-3" aria-hidden="true" />
          Stopping
        </Badge>
      );
    case "waiting":
    case "retry_wait":
      return (
        <Badge variant="warning" size="sm">
          <Clock className="size-3" aria-hidden="true" />
          Waiting
        </Badge>
      );
    case "active":
    case "queued":
    case "running":
      return (
        <Badge variant="info" size="sm">
          <Spinner className="size-3" aria-hidden="true" />
          Running
        </Badge>
      );
  }
}

function agentActivityMetaLabel({
  toolCalls,
  failed,
}: ReturnType<typeof summarizeAgentActivity>): string {
  const count = `${toolCalls} tool ${toolCalls === 1 ? "call" : "calls"}`;
  return failed > 0 ? `${count}, ${failed} failed` : count;
}

/** The current second, ticking only while something on screen depends on it. */
function useNowWhile(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const interval = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(interval);
  }, [active]);
  return now;
}

/**
 * How long the run has been (or was) working, from its durable start to its
 * durable finish — or to now, while it is still live. A run that has not
 * started yet has nothing to count.
 */
function elapsedLabel(run: AgentRun, now: number): string | null {
  if (!run.started_at) return null;
  const started = new Date(run.started_at).getTime();
  if (Number.isNaN(started)) return null;
  const finished = run.finished_at ? new Date(run.finished_at).getTime() : now;
  const seconds = Math.max(0, Math.floor((finished - started) / 1_000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}
