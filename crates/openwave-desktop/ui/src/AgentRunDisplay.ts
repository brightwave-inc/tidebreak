import type { AgentRun } from "./api";

type AgentRunStatus = AgentRun["status"];

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
      web_search: { running: "Searching the web", waiting: "Waiting to search" },
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
      return "Ready for this chat";
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
