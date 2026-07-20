import type { ReactNode } from "react";
import type { AgentRun } from "./api";

const MAX_RECENT_TERMINAL_SANDBOX_RUNS = 2;

export function agentRunsForChat(
  ownerChatId: string | null,
  selectedChatId: string | null,
  runs: AgentRun[],
): AgentRun[] {
  return ownerChatId !== null && ownerChatId === selectedChatId ? runs : [];
}

export function AgentActivityPanel({
  runs,
  loading,
  error,
  onRetry,
}: {
  runs: AgentRun[];
  loading: boolean;
  error: string | null;
  onRetry: () => void;
}) {
  if (loading) {
    return <div className="agent-activity-state">Loading activity…</div>;
  }

  if (error) {
    return (
      <div className="agent-activity-state is-error" role="status">
        Activity unavailable
        <button type="button" className="agent-activity-retry" onClick={onRetry}>
          Retry
        </button>
      </div>
    );
  }

  const foreground = runs.find((run) => run.execution === "foreground");
  const sandboxes = runs.filter((run) => run.execution === "sandbox");
  if (!foreground && sandboxes.length === 0) return null;

  const activeSandboxes = sandboxes.filter((run) =>
    isActiveAgentRunStatus(run.status),
  );
  const terminalSandboxes = sandboxes
    .filter((run) => !isActiveAgentRunStatus(run.status))
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at));
  const recentTerminalSandboxes = terminalSandboxes.slice(
    0,
    MAX_RECENT_TERMINAL_SANDBOX_RUNS,
  );
  const hiddenTerminalCount = terminalSandboxes.length - recentTerminalSandboxes.length;

  return (
    <section className="agent-activity" aria-label="Agent activity">
      <div className="agent-activity-heading">
        <span>Activity</span>
        <span className="agent-activity-summary" aria-live="polite" aria-atomic="true">
          {backgroundSummary(activeSandboxes.length, recentTerminalSandboxes.length)}
        </span>
      </div>

      {foreground && (
        <ul className="agent-activity-list" aria-label="Conversation activity">
          <AgentActivityItem run={foreground} label="Conversation" />
        </ul>
      )}

      {activeSandboxes.length > 0 && (
        <ActivityGroup label="Active" count={activeSandboxes.length} active>
          {activeSandboxes.map((run, index) => (
            <AgentActivityItem
              key={run.id}
              run={run}
              label={`Background task ${index + 1}`}
              announce
            />
          ))}
        </ActivityGroup>
      )}

      {recentTerminalSandboxes.length > 0 && (
        <ActivityGroup label="Recent" count={terminalSandboxes.length}>
          {recentTerminalSandboxes.map((run, index) => (
            <AgentActivityItem
              key={run.id}
              run={run}
              label={`Background task ${index + 1}`}
            />
          ))}
        </ActivityGroup>
      )}

      {hiddenTerminalCount > 0 && (
        <p className="agent-activity-history">
          {hiddenTerminalCount} earlier {hiddenTerminalCount === 1 ? "result" : "results"}
        </p>
      )}
    </section>
  );
}

function ActivityGroup({
  label,
  count,
  active = false,
  children,
}: {
  label: string;
  count: number;
  active?: boolean;
  children: ReactNode;
}) {
  return (
    <section className="agent-activity-group" aria-label={`${label} background tasks`}>
      <div className="agent-activity-group-heading">
        <span className={`agent-activity-group-indicator${active ? " is-active" : ""}`} aria-hidden="true" />
        <span>{label}</span>
        <span className="agent-activity-group-count">{count}</span>
      </div>
      <ul className="agent-activity-list">{children}</ul>
    </section>
  );
}

function AgentActivityItem({
  run,
  label,
  announce = false,
}: {
  run: AgentRun;
  label: string;
  announce?: boolean;
}) {
  const activity = isActiveAgentRunStatus(run.status)
    ? agentActivityPresentation(run.activity)
    : null;
  const status = activity?.status ?? readableAgentRunStatus(run.status);
  return (
    <li
      className={`agent-activity-item is-${run.status}${activity ? ` is-activity-${activity.sourceStatus}` : ""}`}
    >
      <span className="agent-activity-indicator" aria-hidden="true" />
      <span className="agent-activity-item-copy">
        <span>{activity?.label ?? label}</span>
        <span className="agent-activity-detail">
          {activity?.detail ?? agentRunStatusDescription(run.status)}
        </span>
      </span>
      <strong
        aria-live={announce ? "polite" : undefined}
        aria-atomic={announce ? "true" : undefined}
        aria-label={announce ? `${activity?.label ?? label}: ${status}` : undefined}
      >
        {status}
      </strong>
    </li>
  );
}

function backgroundSummary(activeCount: number, recentCount: number): string {
  if (activeCount > 0) {
    return `${activeCount} background ${activeCount === 1 ? "task" : "tasks"}`;
  }
  return recentCount > 0 ? "Recent background work" : "No background work";
}

function agentActivityPresentation(
  activity: AgentRun["activity"],
): { label: string; detail: string; status: string; sourceStatus: string } | null {
  if (!activity) return null;

  const presentations = {
    web_search: {
      label: "Web search",
      waiting: "Waiting to search",
      running: "Searching the web",
      activeStatus: "searching",
    },
    list_connected_folders: {
      label: "Connected folders",
      waiting: "Waiting to check connected folders",
      running: "Checking connected folders",
      activeStatus: "checking",
    },
    list_folder: {
      label: "Folder",
      waiting: "Waiting to list a folder",
      running: "Listing a folder",
      activeStatus: "listing",
    },
    read_connected_file: {
      label: "File",
      waiting: "Waiting to read a file",
      running: "Reading a file",
      activeStatus: "reading",
    },
  } as const;
  const presentation = presentations[activity.kind as keyof typeof presentations];
  if (
    !presentation ||
    (activity.status !== "waiting" && activity.status !== "running")
  ) {
    return null;
  }

  return {
    label: presentation.label,
    detail: presentation[activity.status],
    status: activity.status === "running" ? presentation.activeStatus : "waiting",
    sourceStatus: activity.status,
  };
}

function readableAgentRunStatus(status: AgentRun["status"]): string {
  switch (status) {
    case "retry_wait":
      return "retrying";
    case "cancelling":
      return "stopping";
    default:
      return status;
  }
}

function isActiveAgentRunStatus(status: AgentRun["status"]): boolean {
  return ["active", "queued", "running", "cancelling", "waiting", "retry_wait"].includes(
    status,
  );
}

function agentRunStatusDescription(status: AgentRun["status"]): string {
  switch (status) {
    case "active":
      return "Ready for this conversation";
    case "queued":
      return "Queued to start";
    case "running":
      return "Working in the background";
    case "cancelling":
      return "Stopping";
    case "waiting":
      return "Waiting to continue";
    case "retry_wait":
      return "Waiting to retry";
    case "completed":
      return "Finished";
    case "failed":
      return "Could not finish";
    case "cancelled":
      return "Stopped";
  }
}
