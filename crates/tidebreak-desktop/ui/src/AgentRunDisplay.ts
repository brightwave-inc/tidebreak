import type { AgentActivityHistoryEntry, AgentRun } from "./api";

type AgentRunStatus = AgentRun["status"];
type AgentActivityKind = AgentActivityHistoryEntry["kind"];
type AgentActivityOutcome = AgentActivityHistoryEntry["outcome"];

export type AgentRunStatusGroup = {
  id: string;
  label: string;
  statuses: readonly AgentRunStatus[];
};

/**
 * The vocabulary for durable background work. Groups are deliberately ordered
 * by what needs attention now, then by the settled outcome a reader is most
 * likely to look for.
 */
export const AGENT_RUN_STATUS_GROUPS: readonly AgentRunStatusGroup[] = [
  {
    id: "running",
    label: "Running",
    statuses: ["active", "queued", "running", "waiting", "retry_wait"],
  },
  { id: "needs_input", label: "Needs input", statuses: ["needs_input"] },
  { id: "stopping", label: "Stopping", statuses: ["cancelling"] },
  { id: "completed", label: "Completed", statuses: ["completed"] },
  { id: "stopped", label: "Stopped", statuses: ["cancelled"] },
  { id: "failed", label: "Failed", statuses: ["failed"] },
];

export const RUNNING_AGENT_STATUSES = new Set<AgentRunStatus>([
  "active",
  "queued",
  "running",
  "waiting",
  "retry_wait",
  "cancelling",
]);

export function getAgentRunDotClass(status: AgentRunStatus): string {
  switch (status) {
    case "active":
    case "queued":
    case "running":
    case "waiting":
    case "retry_wait":
    case "cancelling":
      return "animate-pulse bg-muted-foreground";
    case "needs_input":
      return "bg-warning";
    case "completed":
      return "bg-success";
    case "failed":
    case "cancelled":
      return "bg-destructive";
  }
}

export function agentRunStatusDetail(run: AgentRun): string {
  if (run.activity) {
    const activity = {
      exec: { running: "Running a command", waiting: "Waiting to run a command" },
      web_search: { running: "Searching the web", waiting: "Waiting to search" },
      update_task_plan: { running: "Updating its plan", waiting: "Waiting to update its plan" },
      read_delegated_file: {
        running: "Reading a delegated file",
        waiting: "Waiting to read a delegated file",
      },
      list_connected_folders: {
        running: "Checking connected folders",
        waiting: "Waiting to check connected folders",
      },
      list_folder: { running: "Listing a folder", waiting: "Waiting to list a folder" },
      read_connected_file: { running: "Reading a file", waiting: "Waiting to read a file" },
      import_connected_file: { running: "Adding a source", waiting: "Waiting to add a source" },
    } as const;
    const presentation = activity[run.activity.kind];
    if (presentation) return presentation[run.activity.status];
  }

  switch (run.status) {
    case "active":
      return "Ready for this work";
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
    case "needs_input":
      return "Checked in — needs your direction";
    case "completed":
      return "Finished";
    case "failed":
      return "Could not finish";
    case "cancelled":
      return "Stopped";
  }
}

/**
 * One line for a settled or live step in a run's activity history, phrased by
 * outcome. Every phrase is a closed, renderer-safe label — it names what kind
 * of step this was, never what it read, searched for, or found.
 */
const ACTIVITY_HISTORY_LABELS: Record<
  AgentActivityKind,
  Record<AgentActivityOutcome, string>
> = {
  exec: {
    waiting: "Waiting to run a command",
    running: "Running a command",
    completed: "Ran a command",
    failed: "A command failed",
    cancelled: "Command stopped",
  },
  web_search: {
    waiting: "Waiting to search the web",
    running: "Searching the web",
    completed: "Searched the web",
    failed: "Web search failed",
    cancelled: "Web search stopped",
  },
  update_task_plan: {
    waiting: "Waiting to update its plan",
    running: "Updating its plan",
    completed: "Updated its plan",
    failed: "Could not update its plan",
    cancelled: "Plan update stopped",
  },
  read_delegated_file: {
    waiting: "Waiting to read a delegated file",
    running: "Reading a delegated file",
    completed: "Read a delegated file",
    failed: "Could not read a delegated file",
    cancelled: "File read stopped",
  },
  list_connected_folders: {
    waiting: "Waiting to check connected folders",
    running: "Checking connected folders",
    completed: "Checked connected folders",
    failed: "Could not check connected folders",
    cancelled: "Folder check stopped",
  },
  list_folder: {
    waiting: "Waiting to list a folder",
    running: "Listing a folder",
    completed: "Listed a folder",
    failed: "Could not list a folder",
    cancelled: "Folder listing stopped",
  },
  read_connected_file: {
    waiting: "Waiting to read a file",
    running: "Reading a file",
    completed: "Read a file",
    failed: "Could not read a file",
    cancelled: "File read stopped",
  },
  import_connected_file: {
    waiting: "Waiting to add a source",
    running: "Adding a source",
    completed: "Added a source",
    failed: "Could not add a source",
    cancelled: "Adding a source stopped",
  },
};

export function agentActivityHistoryLabel(
  entry: AgentActivityHistoryEntry,
): string {
  return ACTIVITY_HISTORY_LABELS[entry.kind][entry.outcome];
}

export function getAgentActivityOutcomeDotClass(
  outcome: AgentActivityOutcome,
): string {
  switch (outcome) {
    case "waiting":
    case "running":
      return "animate-pulse bg-muted-foreground";
    case "completed":
      return "bg-success";
    case "failed":
    case "cancelled":
      return "bg-destructive";
  }
}
