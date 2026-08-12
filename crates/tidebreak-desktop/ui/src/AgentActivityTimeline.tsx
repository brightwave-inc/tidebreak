import { useEffect, useId, useState } from "react";
import { Check, ChevronRight, Clock, Terminal, X } from "lucide-react";

import type { AgentActivityHistoryEntry } from "./api";
import { agentActivityHistoryLabel } from "./AgentRunDisplay";
import { ToolCardShell } from "./ToolCardShell";
import { ToolIcon } from "./ToolIcon";
import { execCommandHeadline } from "./ToolPreview";
import { ToolStatusIcon, type ToolTone } from "./ToolStatusIcon";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
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
            const execDetail =
              entry.detail?.kind === "exec" ? entry.detail : undefined;
            const headline =
              execDetail === undefined ? activityHeadline(entry.detail) : null;
            return (
              <li
                key={`${entry.at}:${index}`}
                className="flex min-w-0 flex-col gap-1.5"
              >
                <div className="flex min-w-0 items-center gap-1.5">
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
                      headline === null && "flex-1",
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
                        tone === "running" && "animate-pulse",
                      )}
                    >
                      {headline.text}
                    </span>
                  )}
                  <time
                    className="ml-auto shrink-0 text-xs tabular-nums text-muted-foreground"
                    dateTime={entry.at}
                  >
                    {formatActivityTime(entry.at)}
                  </time>
                </div>
                {execDetail !== undefined && (
                  <ExecActivityCard detail={execDetail} outcome={entry.outcome} />
                )}
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
 * detail is what tells the steps apart. Non-exec steps carry it inline; a
 * command gets a full card instead ({@link ExecActivityCard}), so this returns
 * nothing for exec detail.
 */
function activityHeadline(
  detail: AgentActivityHistoryEntry["detail"],
): { text: string; monospace: boolean } | null {
  if (detail === undefined) return null;
  switch (detail.kind) {
    case "exec":
      return null;
    case "search":
      return { text: detail.query, monospace: false };
    case "file":
      return { text: detail.name, monospace: false };
  }
}

type ExecActivityDetail = Extract<
  NonNullable<AgentActivityHistoryEntry["detail"]>,
  { kind: "exec" }
>;

/**
 * A command step's card: the same collapsed chrome a foreground exec card
 * uses — monospace command, outcome pill, chevron. The body holds the full
 * unelided command line and, once the step has settled, the tail of what the
 * command printed; a failed card starts open so the reader lands on what
 * failed.
 */
function ExecActivityCard({
  detail,
  outcome,
}: {
  detail: ExecActivityDetail;
  outcome: AgentActivityHistoryEntry["outcome"];
}) {
  const headline = execCommandHeadline(detail.command, detail.args);
  const failed = outcome === "failed";
  const settled =
    outcome === "completed" || outcome === "failed" || outcome === "cancelled";
  return (
    <ToolCardShell
      icon={<Terminal className="size-3.5 shrink-0" aria-hidden="true" />}
      title={headline}
      titleClassName="font-mono"
      badge={<ExecActivityBadge outcome={outcome} exitCode={detail.exit_code} />}
      defaultExpanded={failed}
      className={cn(failed && "border-critical/35")}
      label={failed ? `Failed command: ${headline}` : `Command: ${headline}`}
    >
      <div className="flex flex-col gap-1.5 p-2">
        <pre className="bg-muted text-muted-foreground rounded-md p-2 text-xs break-words whitespace-pre-wrap">
          {headline}
        </pre>
        {detail.output ? (
          <pre className="bg-muted text-muted-foreground rounded-md p-2 text-xs break-words whitespace-pre-wrap">
            {detail.output}
          </pre>
        ) : (
          // Only a settled step can be said to have printed nothing. While it
          // is still running, an empty pane would read as a finished command
          // that said nothing.
          settled && (
            <p className="text-muted-foreground px-0.5 text-[11px]">
              No output captured.
            </p>
          )
        )}
      </div>
    </ToolCardShell>
  );
}

/**
 * The card's outcome pill, mirroring the foreground exec badge vocabulary. A
 * recorded non-zero exit outranks the generic "failed", exactly as it does on
 * the foreground card.
 */
function ExecActivityBadge({
  outcome,
  exitCode,
}: {
  outcome: AgentActivityHistoryEntry["outcome"];
  exitCode: number | undefined;
}) {
  switch (outcome) {
    case "running":
      return (
        <Badge variant="outline" className="shrink-0 gap-1">
          <Spinner className="size-3" aria-hidden="true" />
          Running…
        </Badge>
      );
    case "waiting":
      return (
        <Badge variant="outline" className="shrink-0 gap-1">
          <Clock className="size-3" aria-hidden="true" />
          Waiting
        </Badge>
      );
    case "completed":
      return (
        <Badge variant="success" className="shrink-0 gap-1">
          <Check className="size-3" aria-hidden="true" />
          Done
        </Badge>
      );
    case "failed":
      return (
        <Badge variant="outline" className="text-critical shrink-0 gap-1">
          <X className="size-3" aria-hidden="true" />
          {exitCode === undefined ? "Failed" : `Exit ${exitCode}`}
        </Badge>
      );
    case "cancelled":
      return (
        <Badge variant="outline" className="text-muted-foreground shrink-0 gap-1">
          <X className="size-3" aria-hidden="true" />
          Stopped
        </Badge>
      );
  }
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

/** The clock time one recorded moment happened, blank if it cannot be read. */
export function formatActivityTime(at: string): string {
  const parsed = new Date(at);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
