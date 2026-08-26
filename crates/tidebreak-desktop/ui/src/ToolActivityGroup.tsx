import { useState, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";
import type { ToolActionPreview, ToolResultPreview } from "./api";
import { ErrorBoundary } from "./ErrorBoundary";
import { toolCallPresentation, type ToolCallStatus } from "./ToolCallCard";
import { ToolEntriesList } from "./ToolEntriesList";
import { ToolIcon } from "./ToolIcon";
import { toolPreviewHeadline } from "./ToolPreview";
import { ToolStatusIcon } from "./ToolStatusIcon";
import { LiveLabel } from "./LiveLabel";
import { useTypewriterOnce } from "./useTypewriterOnce";
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
  /** The tool's own closed view of what it is doing, when it has one. */
  preview?: ToolActionPreview | null;
  /** What the call produced, rendered under the row when it is a list. */
  result?: ToolResultPreview | null;
  /** Set when a retained projection no longer parses against this build. */
  resultUnreadable?: boolean;
};

type ToolActivityGroupProps = {
  activities: ToolActivity[];
  /** Latest assistant snapshot, used only for a live phase's collapsed label. */
  labelActivities?: ToolActivity[];
  /** Transcript message ids that navigation may land on at this phase. */
  anchorIds?: readonly string[];
  groupIndex: number;
  /** Whether this active phase arrived live rather than from socket replay. */
  animate?: boolean;
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
  labelActivities,
  anchorIds = [],
  groupIndex,
  animate = true,
  children,
}: ToolActivityGroupProps) {
  // Total by construction: an activity that throws while being read costs its
  // own row, leaving the rail and the cards below it standing.
  const safeActivities = normalizeActivities(activities);

  // The rail reads model-shaped data through several defensive parsers; the
  // cards below it are the part a reader may have to act on. Containing the
  // rail on its own means a phase line that cannot render costs the line and
  // not the approval prompt sitting under it.
  const rail =
    safeActivities.length === 0 ? null : (
      <ErrorBoundary
        resetKey={railSignature(safeActivities)}
        fallback={<ToolActivityUnavailable />}
      >
        <ToolActivityRail
          activities={safeActivities}
          labelActivities={normalizeActivities(labelActivities ?? activities)}
          groupIndex={groupIndex}
          animate={animate}
        />
      </ErrorBoundary>
    );

  // A phase can be all cards and no rail — every call in it parked on an
  // approval, say. The cards are the part that must never go missing, so they
  // render on their own rather than taking the trigger down with them.
  if (rail === null && children === undefined) {
    return (
      <p className="text-muted-foreground self-start text-sm" role="status">
        Tool activity unavailable
      </p>
    );
  }

  return (
    <div className="w-full self-start">
      {anchorIds.map((anchorId) => (
        <span
          key={anchorId}
          className="transcript-anchor"
          data-transcript-anchor={anchorId}
          aria-hidden="true"
        />
      ))}
      {rail}
      <div
        className={cn(
          "flex flex-col gap-2 empty:hidden",
          rail !== null && "mt-2",
        )}
      >
        {children}
      </div>
    </div>
  );
}

/**
 * The collapsed phase line and, once opened, the rail of rows beneath it.
 */
function ToolActivityRail({
  activities: safeActivities,
  labelActivities: safeLabelActivities,
  groupIndex,
  animate,
}: {
  activities: (ToolActivity | null)[];
  labelActivities: (ToolActivity | null)[];
  groupIndex: number;
  animate: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const contentId = `tool-activity-group-${groupIndex}`;
  // The phase line speaks only for the rows that could be read; an unreadable
  // one is a gap in the rail, not a claim about what the agent did.
  const summary = toolActivityGroupPresentation(
    safeActivities.filter(
      (activity): activity is ToolActivity => activity !== null,
    ),
    safeLabelActivities.filter(
      (activity): activity is ToolActivity => activity !== null,
    ),
  );
  // The phase line types itself out once when the phase first goes live, then
  // updates instantly as calls settle and nudge the wording — re-typing on
  // every change reads as a stutter, and a settled phase should never animate.
  const displayedSummary = useTypewriterOnce(
    summary.label,
    summary.inProgress && animate,
  );

  return (
    <>
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
          aria-label={summary.label}
        >
          <LiveLabel live={summary.inProgress} aria-hidden="true">
            {displayedSummary}
          </LiveLabel>
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
          {/* One boundary per row, not one per phase: a result this build
              cannot render should cost its own line and leave the rest of the
              rail — and the cards under it — standing. */}
          {safeActivities.map((activity, index) =>
            activity === null ? (
              // An activity that could not even be read. It says so in place
              // rather than leaving a gap, which would read as a step that
              // never happened — a different and untrue claim.
              <ToolActivityUnavailable
                key={`position:${index}`}
                role="listitem"
              />
            ) : (
              <ErrorBoundary
                key={activity.id ?? `position:${index}`}
                resetKey={rowSignature(activity)}
                fallback={<ToolActivityUnavailable role="listitem" />}
              >
                <ToolActivityRow activity={activity} />
              </ErrorBoundary>
            ),
          )}
        </div>
      )}
    </>
  );
}

/**
 * What stands in for a row, a rail, or a card that threw while rendering.
 *
 * Deliberately quiet and deliberately not blank: a gap reads as a step that
 * never happened, which is a different and untrue claim.
 */
export function ToolActivityUnavailable({
  role = "status",
}: {
  role?: string;
}) {
  return (
    <p className="tool-activity-unavailable" role={role}>
      This step could not be displayed.
    </p>
  );
}

/**
 * The data a row draws on, reduced to what decides whether it can render.
 *
 * A row that threw is retried when its call moves on or its result arrives,
 * and left alone while the transcript re-renders around it.
 *
 * Total: the rail's signature is computed in the group's own body, outside
 * the rail's boundary, and the preview and result an activity carries are
 * still the projection's own references — a read that throws there costs the
 * row's place in the signature, not the phase.
 */
function rowSignature(activity: ToolActivity): string {
  try {
    return [
      activity.name,
      activity.status,
      activity.resultUnreadable === true ? "unreadable" : "",
      activity.preview?.tool ?? "",
      activity.result?.tool ?? "",
    ].join(" ");
  } catch {
    return "unreadable";
  }
}

function railSignature(activities: readonly (ToolActivity | null)[]): string {
  return activities
    .map((activity) =>
      activity === null ? "unreadable" : rowSignature(activity),
    )
    .join("|");
}

function ToolActivityRow({ activity }: { activity: ToolActivity }) {
  const presentation = toolCallPresentation(activity.name, activity.status);
  // The action under the title, the way the group's cards say it: a search
  // row reads as "Searched sources" over its query, or over the call's own
  // sentence about it when it wrote one. A command's preview stays off the
  // rail — its card below the group already leads with it.
  const headline =
    activity.preview && activity.preview.tool !== "exec"
      ? toolPreviewHeadline(activity.preview).text
      : null;

  return (
    <div className="grid gap-1" role="listitem">
      <div className="flex items-center gap-1.5 font-medium">
        {/* Pinned onto the rail, knocking out the border behind it, so the row
            reads as a stop on the timeline rather than a bullet sitting beside
            one. */}
        <div
          className="text-muted-foreground bg-background absolute -left-px -translate-x-1/2 [&_svg]:size-4"
          aria-hidden="true"
        >
          <ToolIcon name={activity.name} />
        </div>
        <ToolStatusIcon tone={presentation.tone} />
        {/* Row titles do not type — only the phase line does. A running row
            keeps the shimmer so the rail still shows where the work is. */}
        <p className="whitespace-nowrap">
          <LiveLabel live={presentation.tone === "running"}>
            {presentation.title}
          </LiveLabel>
        </p>
      </div>
      {headline !== null && (
        <p className="text-muted-foreground truncate">{headline}</p>
      )}
      {/* A call whose retained projection this build can no longer read.
          Saying so beats rendering nothing: the alternative reads as a call
          that produced no result, which is a different and untrue claim. */}
      {activity.resultUnreadable ? (
        <p className="text-muted-foreground text-xs" role="status">
          This tool completed, but its result can no longer be displayed.
        </p>
      ) : activity.result?.tool === "entries" ? (
        <ToolEntriesList name={activity.name} result={activity.result} />
      ) : null}
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

const CATEGORY_ORDER: Category[] = ["wait", "spawn", "other"];

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
  labelActivities: readonly ToolActivity[] = activities,
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
  const labelPresentations = (
    inProgress && labelActivities.length > 0 ? labelActivities : activities
  ).map((activity) => ({
    activity,
    ...toolCallPresentation(activity.name, activity.status),
  }));
  const counts: Record<Category, number> = { wait: 0, spawn: 0, other: 0 };
  for (const { activity } of labelPresentations) {
    counts[categoryOf(activity.name)] += 1;
  }

  const latest = labelPresentations[labelPresentations.length - 1]!;
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

/**
 * The activities, reduced to plain data the rail can read without throwing.
 *
 * `null` marks one that threw while being read — retained projections can
 * carry getters this build no longer agrees with. It stays in the list as a
 * placeholder row rather than being dropped: a gap reads as a step that never
 * happened, which is a different and untrue claim. Reading happens here, in
 * the group's own body outside the rail's boundary, so a throw that escaped
 * would fall to the phase's backstop and take the phase's cards down with it;
 * per activity, the damage stops at the row.
 */
function normalizeActivities(activities: unknown): (ToolActivity | null)[] {
  if (!Array.isArray(activities)) return [];
  return activities.flatMap((activity) => {
    try {
      const normalized = normalizeActivity(activity);
      return normalized === null ? [] : [normalized];
    } catch (error) {
      console.error("tool activity could not be read", error);
      return [null];
    }
  });
}

/** One activity as plain data, or `null` when it is not shaped like one. */
function normalizeActivity(activity: unknown): ToolActivity | null {
  const candidate = activity as Record<string, unknown> | null;
  if (
    candidate === null ||
    typeof candidate !== "object" ||
    typeof candidate.name !== "string" ||
    typeof candidate.status !== "string"
  ) {
    return null;
  }
  return {
    // Carried through rather than dropped: it is what keys the row.
    ...(typeof candidate.id === "string" && candidate.id.length > 0
      ? { id: candidate.id }
      : {}),
    name: candidate.name,
    status: candidate.status as ToolCallStatus,
    // Already validated field by field at the API boundary; carried here
    // so the expanded row can say what the call did and what it found.
    ...(typeof candidate.preview === "object"
      ? { preview: candidate.preview as ToolActionPreview | null }
      : {}),
    ...(typeof candidate.result === "object"
      ? { result: candidate.result as ToolResultPreview | null }
      : {}),
    ...(typeof candidate.resultUnreadable === "boolean"
      ? { resultUnreadable: candidate.resultUnreadable }
      : {}),
  };
}

function lowercaseFirst(value: string): string {
  return value.length === 0 ? value : value[0]!.toLowerCase() + value.slice(1);
}
