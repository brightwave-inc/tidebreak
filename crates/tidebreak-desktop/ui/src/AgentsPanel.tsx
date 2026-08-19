import { Bot } from "lucide-react";

import { RUNNING_AGENT_STATUSES } from "./AgentRunDisplay";
import type { AgentRun } from "./api";
import {
  AgentRunStatusBadge,
  elapsedLabel,
  useNowWhile,
} from "./BackgroundAgentPanel";
import { PanelBreadcrumb } from "@/components/PanelHeader";
import { PanelFrame } from "@/panel/PanelFrame";
import { Skeleton } from "@/components/ui/skeleton";

/**
 * The chat's background agents, as a table — every run this conversation has
 * spawned, newest first, with a row opening that run's own panel.
 *
 * Presentational on purpose: the route already observes the runs for the
 * status chip, so the table reads the same list rather than starting a second
 * poller for the same snapshot.
 */
export function AgentsPanel({
  runs,
  loading,
  error,
  onRetry,
  onOpenRun,
}: {
  runs: readonly AgentRun[];
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  onOpenRun: (runId: string) => void;
}) {
  return (
    <PanelFrame breadcrumb={<PanelBreadcrumb firstPart="Agents" />} showBorder>
      {runs.length > 0 ? (
        <AgentsTable runs={runs} onOpenRun={onOpenRun} />
      ) : error ? (
        <div className="flex items-center justify-between gap-3 p-4 text-sm" role="status">
          <span>Background agent status is unavailable.</span>
          <button
            type="button"
            className="shrink-0 font-medium text-primary hover:underline"
            onClick={onRetry}
          >
            Retry
          </button>
        </div>
      ) : loading ? (
        <div className="flex flex-col gap-3 p-4" role="status" aria-label="Loading agents">
          <Skeleton className="h-5 w-48" />
          <Skeleton className="h-5 w-64" />
          <Skeleton className="h-5 w-56" />
        </div>
      ) : (
        <div className="flex flex-col items-center gap-2 p-8 text-center">
          <Bot className="text-icon-violet size-6" aria-hidden="true" />
          <p className="text-sm font-medium">No background agents yet</p>
          <p className="text-sm text-muted-foreground">
            Ask for something to be worked on in the background and its agent
            will show up here.
          </p>
        </div>
      )}
    </PanelFrame>
  );
}

function AgentsTable({
  runs,
  onOpenRun,
}: {
  runs: readonly AgentRun[];
  onOpenRun: (runId: string) => void;
}) {
  // Elapsed time only moves while something in the table is live.
  const anyLive = runs.some((run) => RUNNING_AGENT_STATUSES.has(run.status));
  const now = useNowWhile(anyLive);
  const newestFirst = runs.slice().reverse();

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <table className="w-full border-collapse text-sm">
        <thead className="sticky top-0 bg-background">
          <tr className="border-b text-left text-xs text-muted-foreground">
            <th scope="col" className="px-4 py-2 font-medium">
              Agent
            </th>
            <th scope="col" className="w-28 px-2 py-2 font-medium">
              Status
            </th>
            <th scope="col" className="w-20 px-4 py-2 text-right font-medium">
              Time
            </th>
          </tr>
        </thead>
        <tbody>
          {newestFirst.map((run) => (
            <tr
              key={run.id}
              className="cursor-pointer border-b transition-colors last:border-b-0 hover:bg-accent"
              onClick={() => onOpenRun(run.id)}
            >
              <td className="max-w-0 px-4 py-2.5">
                <button
                  type="button"
                  className="block w-full cursor-pointer truncate text-left font-medium"
                  onClick={(event) => {
                    // The row already opens the run; without this the click
                    // would bubble and open it a second time.
                    event.stopPropagation();
                    onOpenRun(run.id);
                  }}
                >
                  {run.task ?? "Background agent"}
                </button>
              </td>
              <td className="px-2 py-2.5">
                <AgentRunStatusBadge status={run.status} />
              </td>
              <td className="px-4 py-2.5 text-right text-xs whitespace-nowrap text-muted-foreground">
                {elapsedLabel(run, now) ?? "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
