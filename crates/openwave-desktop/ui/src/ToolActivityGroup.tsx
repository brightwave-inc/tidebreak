import { useState } from "react";
import {
  toolCallPresentation,
  type ToolCallStatus,
} from "./ToolCallCard";

export type ToolActivity = {
  id?: string;
  name: string;
  status: ToolCallStatus;
};

type ToolActivityGroupProps = {
  activities: ToolActivity[];
  groupIndex: number;
};

export function ToolActivityGroup({
  activities,
  groupIndex,
}: ToolActivityGroupProps) {
  const [expanded, setExpanded] = useState(false);
  const contentId = `tool-activity-group-${groupIndex}`;
  const safeActivities = normalizeActivities(activities);
  const summary = toolActivityGroupPresentation(safeActivities);

  if (safeActivities.length === 0) {
    return (
      <p className="tool-activity-unavailable" role="status">
        Tool activity unavailable
      </p>
    );
  }

  return (
    <section
      className={`tool-activity-group is-${summary.phase}`}
      aria-label={
        summary.phase === "settled"
          ? "Completed tool activity"
          : "Active tool activity"
      }
    >
      <button
        type="button"
        className="tool-activity-group-toggle"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span
          className={`tool-activity-group-status is-${summary.tone}`}
          aria-hidden="true"
        >
          {summary.icon}
        </span>
        <span className="tool-activity-group-label">{summary.label}</span>
        <span
          className={`tool-activity-group-chevron${expanded ? " is-expanded" : ""}`}
          aria-hidden="true"
        >
          ›
        </span>
      </button>
      <div
        id={contentId}
        hidden={!expanded}
        className="tool-activity-group-list"
        role="list"
      >
        {safeActivities.map((activity, index) => {
          const presentation = toolCallPresentation(
            activity.name,
            activity.status,
          );
          return (
            <div
              className={`tool-activity-timeline-item is-${presentation.tone}`}
              role="listitem"
              key={index}
            >
              <span className="tool-activity-timeline-marker" aria-hidden="true">
                {presentation.icon}
              </span>
              <span className="tool-activity-timeline-copy">
                <strong>{presentation.label}</strong>
                <span>{presentation.statusLabel}</span>
              </span>
            </div>
          );
        })}
      </div>
    </section>
  );
}

export type ToolActivityGroupPresentation = {
  phase: "active" | "settled";
  tone: "running" | "completed" | "failed" | "cancelled" | "unknown";
  icon: string;
  label: string;
};

export function toolActivityGroupPresentation(
  activities: readonly ToolActivity[],
): ToolActivityGroupPresentation {
  if (activities.length === 0) {
    return {
      phase: "settled",
      tone: "unknown",
      icon: "?",
      label: "Tool activity unavailable",
    };
  }

  const presentations = activities.map((activity) =>
    toolCallPresentation(activity.name, activity.status),
  );
  const active = presentations.filter(
    ({ tone }) => tone === "running" || tone === "waiting_approval",
  );
  if (active.length > 0) {
    const latest = active[active.length - 1]!;
    return {
      phase: "active",
      tone: "running",
      icon: latest.icon,
      label: latest.statusLabel,
    };
  }

  const completed = presentations.filter(({ tone }) => tone === "completed");
  const failed = presentations.filter(({ tone }) => tone === "failed");
  const cancelled = presentations.filter(({ tone }) => tone === "cancelled");
  const unknown = presentations.filter(({ tone }) => tone === "unknown");

  if (completed.length === presentations.length && completed.length <= 2) {
    return {
      phase: "settled",
      tone: "completed",
      icon: "✓",
      label: completed
        .map(({ settledLabel }, index) =>
          index === 0 ? settledLabel : lowercaseFirst(settledLabel),
        )
        .join(" and "),
    };
  }

  const counts = [
    countLabel(completed.length, "completed"),
    countLabel(failed.length, "failed"),
    countLabel(cancelled.length, "not run"),
    countLabel(unknown.length, "unavailable"),
  ].filter((label): label is string => label !== null);
  const tone =
    failed.length > 0
      ? "failed"
      : unknown.length > 0
        ? "unknown"
        : cancelled.length > 0
          ? "cancelled"
          : "completed";

  return {
    phase: "settled",
    tone,
    icon:
      tone === "failed"
        ? "!"
        : tone === "cancelled"
          ? "–"
          : tone === "unknown"
            ? "?"
            : "✓",
    label: `${activities.length} tool ${
      activities.length === 1 ? "activity" : "activities"
    }${counts.length > 0 ? ` · ${counts.join(" · ")}` : ""}`,
  };
}

function countLabel(count: number, label: string): string | null {
  return count > 0 ? `${count} ${label}` : null;
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
        name: candidate.name,
        status: candidate.status as ToolCallStatus,
      },
    ];
  });
}

function lowercaseFirst(value: string): string {
  return value.length === 0 ? value : value[0]!.toLowerCase() + value.slice(1);
}
