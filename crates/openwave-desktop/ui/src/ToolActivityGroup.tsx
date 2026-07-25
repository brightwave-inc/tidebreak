import { useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";
import { toolCallPresentation, type ToolCallStatus } from "./ToolCallCard";
import { ToolIcon } from "./ToolIcon";
import { ToolStatusIcon } from "./ToolStatusIcon";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

export type ToolActivity = {
  /**
   * Stable identity for this row.
   *
   * Rows were keyed by array index, so filtering or reordering a phase
   * re-associated React state with the wrong call. Callers pass a transcript
   * entry id; a row that arrives without one falls back to its position, which
   * is no worse than before and no better.
   */
  id?: string;
  name: string;
  status: ToolCallStatus;
};

type ToolActivityGroupProps = {
  activities: ToolActivity[];
  groupIndex: number;
  /** Cards for the calls that have something to show, rendered below. */
  children?: ReactNode;
};

/**
 * One phase of the agent's work.
 *
 * Every tool call in a contiguous run lives here, behind one line of text that
 * names what the phase is doing. A transcript should read as a conversation
 * with occasional notes about what the agent did, not as a log — so the default
 * is a single muted line, and the detail is one click away.
 *
 * Calls that produced something worth seeing render their own card underneath,
 * outside the collapsed region: a card whose controls can be hidden is a card
 * that can be missed.
 */
export function ToolActivityGroup({
  activities,
  groupIndex,
  children,
}: ToolActivityGroupProps) {
  const [expanded, setExpanded] = useState(false);
  const contentId = `tool-activity-group-${groupIndex}`;
  const safeActivities = normalizeActivities(activities);
  const summary = toolActivityGroupPresentation(safeActivities);

  // A phase can be all cards and no rail — every call in it parked on an
  // approval, say. The cards are the part that must never go missing, so they
  // render on their own rather than taking the trigger down with them.
  if (safeActivities.length === 0) {
    if (children === undefined) {
      return (
        <p className="text-muted-foreground self-start text-sm" role="status">
          Tool activity unavailable
        </p>
      );
    }
    return (
      <div className="flex w-full flex-col gap-2 self-start empty:hidden">
        {children}
      </div>
    );
  }

  return (
    <div className="w-full self-start">
      <Button
        type="button"
        variant="link"
        className="text-muted-foreground h-auto px-0 py-1"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => setExpanded((current) => !current)}
      >
        <ToolIcon name={summary.iconName} className="size-4" />
        {/* The phase label is the only running commentary on what the agent is
            doing, so it is announced as it changes rather than only on demand. */}
        <span
          role="status"
          aria-live="polite"
          aria-atomic="true"
          className={cn(summary.inProgress && "animate-pulse")}
        >
          {summary.label}
        </span>
        <ChevronDown
          className={cn(
            "size-4 transition-transform",
            !expanded && "-rotate-90",
          )}
        />
      </Button>
      {expanded && (
        <div
          id={contentId}
          className="relative ml-1.5 flex flex-col gap-4 border-l-2 py-1 pl-4 text-sm"
          role="list"
        >
          {safeActivities.map((activity, index) => {
            const presentation = toolCallPresentation(
              activity.name,
              activity.status,
            );
            return (
              <div
                className="grid gap-1"
                role="listitem"
                key={activity.id ?? `position:${index}`}
              >
                <div className="flex items-center gap-1.5 font-medium">
                  {/* Pinned onto the rail, knocking out the border behind it,
                      so the row reads as a stop on the timeline rather than a
                      bullet sitting beside one. */}
                  <div
                    className="text-muted-foreground bg-background absolute -left-px -translate-x-1/2 [&_svg]:size-4"
                    aria-hidden="true"
                  >
                    <ToolIcon name={activity.name} />
                  </div>
                  <ToolStatusIcon tone={presentation.tone} />
                  <p
                    className={cn(
                      "whitespace-nowrap",
                      presentation.tone === "running" && "animate-pulse",
                    )}
                  >
                    {presentation.title}
                  </p>
                </div>
              </div>
            );
          })}
        </div>
      )}
      <div className="mt-2 flex flex-col gap-2 empty:hidden">{children}</div>
    </div>
  );
}

export type ToolActivityGroupPresentation = {
  phase: "active" | "settled";
  tone: "running" | "completed" | "failed" | "cancelled" | "unknown";
  /** Allowlisted tool name whose icon leads the group. */
  iconName: string;
  inProgress: boolean;
  label: string;
};

type Category = "wait" | "spawn" | "other";

/**
 * Aggregating vocabulary for the categories that read better counted than
 * named one by one. Waiting is deduped: how many agents are being waited on is
 * not what the line is about.
 */
const CATEGORY_SPECS: Record<
  Exclude<Category, "other">,
  { verbs: { inProgress: string; complete: string }; noun: string | null }
> = {
  spawn: {
    verbs: { inProgress: "Delegating", complete: "Delegated" },
    noun: "task",
  },
  wait: {
    verbs: {
      inProgress: "Waiting for background agents",
      complete: "Waited for background agents",
    },
    noun: null,
  },
};

const CATEGORY_ORDER: Category[] = ["other", "spawn", "wait"];

function categoryOf(name: string): Category {
  if (name === "spawn_sandbox_agent") return "spawn";
  if (name === "wait_for_agents") return "wait";
  return "other";
}

/**
 * The one line that stands in for a whole phase.
 *
 * It names the most recent thing that happened and counts the rest, because
 * that is what someone scrolling past wants to know. A tally of outcomes
 * ("3 activities · 1 failed") describes the log rather than the work.
 *
 * While the phase is live every category speaks in the present, so the line
 * doesn't flicker between tenses as individual calls settle underneath it.
 */
export function toolActivityGroupPresentation(
  activities: readonly ToolActivity[],
): ToolActivityGroupPresentation {
  if (activities.length === 0) {
    return {
      phase: "settled",
      tone: "unknown",
      iconName: "other",
      inProgress: false,
      label: "Tool activity unavailable",
    };
  }

  const presentations = activities.map((activity) => ({
    activity,
    ...toolCallPresentation(activity.name, activity.status),
  }));
  const inProgress = presentations.some(
    ({ tone }) => tone === "running" || tone === "waiting_approval",
  );
  const counts: Record<Category, number> = { wait: 0, spawn: 0, other: 0 };
  for (const { activity } of presentations) {
    counts[categoryOf(activity.name)] += 1;
  }

  const latest = presentations[presentations.length - 1]!;
  const leadCategory = categoryOf(latest.activity.name);
  const leadPhrase =
    leadCategory === "other"
      ? toolCallPresentation(
          latest.activity.name,
          inProgress ? "running" : latest.activity.status,
        ).title
      : aggregationPhrase(leadCategory, counts[leadCategory], inProgress);

  const appendages: string[] = [];
  for (const category of CATEGORY_ORDER) {
    if (category === "other") {
      const remaining =
        leadCategory === "other" ? counts.other - 1 : counts.other;
      if (remaining > 0) {
        appendages.push(
          `${remaining} other ${remaining === 1 ? "task" : "tasks"}`,
        );
      }
      continue;
    }
    if (category === leadCategory || counts[category] === 0) continue;
    appendages.push(
      lowercaseFirst(aggregationPhrase(category, counts[category], inProgress)),
    );
  }

  const failed = presentations.some(({ tone }) => tone === "failed");
  const unknown = presentations.some(({ tone }) => tone === "unknown");
  const cancelled = presentations.some(({ tone }) => tone === "cancelled");

  return {
    phase: inProgress ? "active" : "settled",
    tone: inProgress
      ? "running"
      : failed
        ? "failed"
        : unknown
          ? "unknown"
          : cancelled
            ? "cancelled"
            : "completed",
    iconName: latest.activity.name,
    inProgress,
    label: joinWithOxford(leadPhrase, appendages),
  };
}

function aggregationPhrase(
  category: Exclude<Category, "other">,
  count: number,
  inProgress: boolean,
): string {
  const spec = CATEGORY_SPECS[category];
  const verb = inProgress ? spec.verbs.inProgress : spec.verbs.complete;
  if (spec.noun === null) return verb;
  return `${verb} ${count} ${count === 1 ? spec.noun : `${spec.noun}s`}`;
}

function joinWithOxford(lead: string, appendages: string[]): string {
  if (appendages.length === 0) return lead;
  if (appendages.length === 1) return `${lead} and ${appendages[0]}`;
  const head = appendages.slice(0, -1).join(", ");
  return `${lead}, ${head}, and ${appendages[appendages.length - 1]}`;
}

function normalizeActivities(activities: unknown): ToolActivity[] {
  if (!Array.isArray(activities)) return [];
  return activities.flatMap((activity) => {
    const candidate = activity as Record<string, unknown> | null;
    if (
      candidate === null ||
      typeof candidate !== "object" ||
      typeof candidate.name !== "string" ||
      typeof candidate.status !== "string"
    ) {
      return [];
    }
    return [
      {
        // Carried through rather than dropped: it is what keys the row.
        ...(typeof candidate.id === "string" && candidate.id.length > 0
          ? { id: candidate.id }
          : {}),
        name: candidate.name,
        status: candidate.status as ToolCallStatus,
      },
    ];
  });
}

function lowercaseFirst(value: string): string {
  return value.length === 0 ? value : value[0]!.toLowerCase() + value.slice(1);
}
