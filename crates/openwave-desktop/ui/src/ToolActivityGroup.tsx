import { useState } from "react";
import {
  ToolCallCard,
  toolCallPresentation,
  type ToolCallStatus,
} from "./ToolCallCard";

type ToolActivity = {
  id: string;
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
  const summary = activities
    .map((activity) => toolCallPresentation(activity.name, activity.status))
    .map(({ label, statusLabel }) => `${label}: ${statusLabel}`)
    .join(" · ");
  const actionLabel = `${activities.length} tool activities`;

  return (
    <section className="tool-activity-group" aria-label="Tool activity">
      <button
        type="button"
        className="tool-activity-group-toggle"
        aria-expanded={expanded}
        aria-controls={contentId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="tool-activity-group-icon" aria-hidden="true">
          {expanded ? "⌄" : "›"}
        </span>
        <span className="tool-activity-group-copy">
          <strong>{actionLabel}</strong>
          <span>{summary}</span>
        </span>
      </button>
      <div
        id={contentId}
        hidden={!expanded}
        className="tool-activity-group-list"
      >
        {activities.map((activity) => (
          <ToolCallCard
            key={activity.id}
            name={activity.name}
            status={activity.status}
          />
        ))}
      </div>
    </section>
  );
}
