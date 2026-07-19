export type ToolCallStatus =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "cancelled";

type ToolCallCardProps = {
  name: string;
  status: ToolCallStatus;
};

type ToolPresentation = {
  label: string;
  active: string;
  complete: string;
};

export type ToolCallPresentation = {
  label: string;
  statusLabel: string;
};

export type ToolApprovalPresentation = {
  summary: string;
  canApprove: boolean;
};

// This is deliberately an allowlist rather than a display transformation of a
// tool name. Tool events come from providers, and a card should never become a
// side channel for an unexpected tool name, arguments, output, file path, or
// provider diagnostic.
const TOOL_PRESENTATIONS: Record<string, ToolPresentation> = {
  search: {
    label: "Search documents",
    active: "Searching documents",
    complete: "Document search complete",
  },
  web_search: {
    label: "Search the web",
    active: "Searching the web",
    complete: "Web search complete",
  },
  read_file: {
    label: "Read a file",
    active: "Reading a file",
    complete: "File read complete",
  },
  list_dir: {
    label: "Browse files",
    active: "Browsing files",
    complete: "File browse complete",
  },
  write_file: {
    label: "Update a file",
    active: "Updating a file",
    complete: "File update complete",
  },
  request_folder_access: {
    label: "Request folder access",
    active: "Requesting folder access",
    complete: "Folder access request complete",
  },
  connect_folder: {
    label: "Connect a folder",
    active: "Connecting a folder",
    complete: "Folder connected",
  },
  list_connected_folders: {
    label: "Check connected folders",
    active: "Checking connected folders",
    complete: "Connected folders checked",
  },
  read_connected_file: {
    label: "Read a connected file",
    active: "Reading a connected file",
    complete: "Connected file read complete",
  },
  spawn_sandbox_agent: {
    label: "Delegate a task",
    active: "Delegating a task",
    complete: "Delegated task complete",
  },
};

const FALLBACK_TOOL: ToolPresentation = {
  label: "Use a tool",
  active: "Using a tool",
  complete: "Tool complete",
};

export function ToolCallCard({ name, status }: ToolCallCardProps) {
  const presentation = toolCallPresentation(name, status);

  return (
    <section
      className={`tool-call-card is-${status}`}
      aria-label={`${presentation.label}: ${presentation.statusLabel}`}
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <span className="tool-call-icon" aria-hidden="true">
        {presentation.icon}
      </span>
      <div className="tool-call-copy">
        <strong>{presentation.label}</strong>
        <span>{presentation.statusLabel}</span>
      </div>
      {status === "running" && (
        <span className="tool-call-spinner" aria-hidden="true" />
      )}
    </section>
  );
}

// Callers that summarize activity must use the same allowlisted presentation
// as the card. In particular, do not transform provider-supplied tool names
// into text for a summary.
export function toolCallPresentation(
  name: string,
  status: ToolCallStatus,
): ToolCallPresentation & { icon: string } {
  const tool = TOOL_PRESENTATIONS[name] ?? FALLBACK_TOOL;
  const statusPresentationResult = statusPresentation(tool, status);

  return {
    label: tool.label,
    statusLabel: statusPresentationResult.label,
    icon: statusPresentationResult.icon,
  };
}

export function toolApprovalPresentation(
  kind: string,
): ToolApprovalPresentation {
  if (kind === "search_may_share_query_and_excerpts") {
    return {
      summary:
        "Allow search to send your query and potentially matching document excerpts to configured AI services outside OpenWave?",
      canApprove: true,
    };
  }
  return {
    summary: "The exact action cannot be safely described.",
    canApprove: false,
  };
}

function statusPresentation(tool: ToolPresentation, status: ToolCallStatus) {
  switch (status) {
    case "waiting_approval":
      return { icon: "…", label: "Waiting for approval" };
    case "completed":
      return { icon: "✓", label: tool.complete };
    case "failed":
      return { icon: "!", label: "Tool could not complete" };
    case "cancelled":
      return { icon: "–", label: "Not run" };
    case "running":
      return { icon: "↗", label: tool.active };
  }
}
