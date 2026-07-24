import { useId, useState } from "react";
import { Check, ChevronDown, Clock, Loader2, Terminal, X } from "lucide-react";
import type { ToolApprovalPreview } from "./api";
import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";
import type { ToolTone } from "./ToolStatusIcon";
import { ScrollableContainer } from "./ScrollableContainer";
import { toolPreviewPresentation } from "./ToolPreview";

export type ToolCallStatus =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "cancelled";

type ToolCommandCardProps = {
  name: string;
  status: ToolCallStatus;
  /** The tool's own view of the command, which is what the card is for. */
  preview: ToolApprovalPreview;
};

type ToolPresentation = {
  label: string;
  active: string;
  complete: string;
  settled: string;
};

export type ToolCallPresentation = {
  label: string;
  /** Tense-aware headline for this status. */
  title: string;
  /** Short status word for the card's badge. */
  badgeLabel: string;
  /** Full status sentence, used for assistive text and activity summaries. */
  statusLabel: string;
  settledLabel: string;
  tone: ToolTone;
};

export type ToolApprovalPresentation = {
  summary: string;
  canApprove: boolean;
  canRemember: boolean;
};

// This is deliberately an allowlist rather than a display transformation of a
// tool name. Tool events come from providers, and a card should never become a
// side channel for an unexpected tool name, arguments, output, file path, or
// provider diagnostic. The one exception is a tool's own `preview`, a closed
// per-tool projection built server-side.
const TOOL_PRESENTATIONS: Record<string, ToolPresentation> = {
  search: {
    label: "Search sources",
    active: "Searching sources",
    complete: "Source search complete",
    settled: "Searched sources",
  },
  list_sources: {
    label: "Check sources",
    active: "Checking sources",
    complete: "Sources checked",
    settled: "Checked sources",
  },
  read_source: {
    label: "Read a source",
    active: "Reading a source",
    complete: "Source read complete",
    settled: "Read a source",
  },
  web_search: {
    label: "Search the web",
    active: "Searching the web",
    complete: "Web search complete",
    settled: "Searched the web",
  },
  read_delegated_file: {
    label: "Read a delegated file",
    active: "Reading a delegated file",
    complete: "Delegated file read complete",
    settled: "Read a delegated file",
  },
  read_file: {
    label: "Read a file",
    active: "Reading a file",
    complete: "File read complete",
    settled: "Read a file",
  },
  list_dir: {
    label: "Browse files",
    active: "Browsing files",
    complete: "File browse complete",
    settled: "Browsed files",
  },
  write_file: {
    label: "Update a file",
    active: "Updating a file",
    complete: "File update complete",
    settled: "Updated a file",
  },
  create_deliverable: {
    label: "Create an output",
    active: "Creating an output",
    complete: "Output ready",
    settled: "Created an output",
  },
  request_folder_access: {
    label: "Request folder access",
    active: "Requesting folder access",
    complete: "Folder access request complete",
    settled: "Requested folder access",
  },
  connect_folder: {
    label: "Connect a folder",
    active: "Connecting a folder",
    complete: "Folder connected",
    settled: "Connected a folder",
  },
  list_connected_folders: {
    label: "Check connected folders",
    active: "Checking connected folders",
    complete: "Connected folders checked",
    settled: "Checked connected folders",
  },
  read_connected_file: {
    label: "Read a connected file",
    active: "Reading a connected file",
    complete: "Connected file read complete",
    settled: "Read a connected file",
  },
  ask_user_questions: {
    label: "Ask a question",
    active: "Waiting for your answer",
    complete: "Answer received",
    settled: "Asked a question",
  },
  spawn_sandbox_agent: {
    label: "Delegate a task",
    active: "Delegating a task",
    complete: "Task delegated",
    settled: "Task delegated",
  },
  wait_for_agents: {
    label: "Wait for background agents",
    active: "Waiting for background agents",
    complete: "Background agents finished",
    settled: "Background agents finished",
  },
  exec: {
    label: "Run a command",
    active: "Running a command",
    complete: "Command complete",
    settled: "Ran a command",
  },
};

const FALLBACK_TOOL: ToolPresentation = {
  label: "Use a tool",
  active: "Using a tool",
  complete: "Tool complete",
  settled: "Used a tool",
};

/**
 * The card for a command the agent ran.
 *
 * The command is the title, in monospace: "Ran a command" is the same sentence
 * whether the agent listed a directory or rebuilt the workspace, so the only
 * useful headline is the command itself. It opens while the command is still
 * running — that is when someone is actually watching — and collapses back to
 * one line once there is an outcome to read in the badge.
 *
 * Only tools that project a preview get a card. Everything else lives in the
 * activity rail, where a line of text is the whole story the renderer has.
 */
export function ToolCommandCard({ name, status, preview }: ToolCommandCardProps) {
  const presentation = toolCallPresentation(name, status);
  const command = toolPreviewPresentation(preview);
  const running = presentation.tone === "running";
  const [expanded, setExpanded] = useState(running);
  const bodyId = useId();

  return (
    <section
      className="bg-background max-w-prose overflow-hidden rounded-lg border"
      aria-label={`${presentation.label}: ${presentation.statusLabel}`}
      role="status"
      aria-live="polite"
      aria-atomic="true"
    >
      <button
        type="button"
        className="hover:bg-muted/50 focus-visible:ring-ring flex w-full items-center justify-between gap-2 px-2.5 py-1.5 text-left transition-colors focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-hidden"
        aria-expanded={expanded}
        aria-controls={bodyId}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="text-muted-foreground flex min-w-0 items-center gap-1.5 text-xs font-medium">
          <ChevronDown
            className={cn(
              "size-3.5 shrink-0 transition-transform",
              !expanded && "-rotate-90",
            )}
            aria-hidden="true"
          />
          <Terminal className="size-3.5 shrink-0" aria-hidden="true" />
          <span className="truncate font-mono">{command.headline}</span>
        </span>
        <ToolStatusBadge presentation={presentation} />
      </button>
      {expanded && (
        <div id={bodyId} className="border-t p-1">
          <ScrollableContainer className="bg-muted text-muted-foreground rounded-md p-2 text-xs whitespace-pre-wrap">
            {command.detail}
          </ScrollableContainer>
        </div>
      )}
    </section>
  );
}

function ToolStatusBadge({
  presentation,
}: {
  presentation: ToolCallPresentation;
}) {
  if (presentation.tone === "running") {
    return (
      <Badge variant="outline" className="shrink-0 gap-1">
        <Loader2 className="size-3 animate-spin" aria-hidden="true" />
        Running…
      </Badge>
    );
  }
  if (presentation.tone === "completed") {
    return (
      <Badge variant="success" className="shrink-0 gap-1">
        <Check className="size-3" aria-hidden="true" />
        Done
      </Badge>
    );
  }
  if (presentation.tone === "waiting_approval") {
    return (
      <Badge variant="outline" className="shrink-0 gap-1">
        <Clock className="size-3" aria-hidden="true" />
        Waiting for approval
      </Badge>
    );
  }
  return (
    <Badge
      variant="outline"
      className={cn(
        "text-muted-foreground shrink-0 gap-1",
        presentation.tone === "failed" && "text-destructive",
      )}
    >
      <X className="size-3" aria-hidden="true" />
      {presentation.badgeLabel}
    </Badge>
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
    title: statusPresentationResult.title,
    badgeLabel: statusPresentationResult.badge,
    statusLabel: statusPresentationResult.label,
    settledLabel: tool.settled,
    tone: statusPresentationResult.tone,
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
      canRemember: true,
    };
  }
  if (kind === "web_search_may_share_query") {
    return {
      summary:
        "Allow web search to send this query and its explicit filters to the configured search provider outside OpenWave?",
      canApprove: true,
      canRemember: true,
    };
  }
  if (kind === "exec_may_run_networked_command") {
    return {
      summary:
        "Allow OpenWave to run a command that leaves the chat workspace and may reach the network?",
      canApprove: true,
      canRemember: true,
    };
  }
  if (kind === "external_mcp_may_call_server") {
    return {
      summary:
        "Allow this external MCP server to receive the call and act with its own local or remote permissions?",
      canApprove: true,
      // The executable behind a stable MCP namespace can change in Settings.
      canRemember: false,
    };
  }
  return {
    summary: "The exact action cannot be safely described.",
    canApprove: false,
    canRemember: false,
  };
}

function statusPresentation(tool: ToolPresentation, status: ToolCallStatus) {
  switch (status) {
    case "waiting_approval":
      return {
        icon: "…",
        title: tool.label,
        badge: "Waiting for approval",
        label: "Waiting for approval",
        tone: "waiting_approval" as const,
      };
    case "completed":
      return {
        icon: "✓",
        title: tool.settled,
        badge: "Done",
        label: tool.complete,
        tone: "completed" as const,
      };
    case "failed":
      return {
        icon: "!",
        // A failure keeps the untensed phrase: the tool did not do the thing,
        // so naming it in the past would overstate what happened.
        title: tool.label,
        badge: "Failed",
        label: "Tool could not complete",
        tone: "failed" as const,
      };
    case "cancelled":
      return {
        icon: "–",
        title: tool.label,
        badge: "Not run",
        label: "Not run",
        tone: "cancelled" as const,
      };
    case "running":
      return {
        icon: "↗",
        title: tool.active,
        badge: "Running…",
        label: tool.active,
        tone: "running" as const,
      };
  }

  // Renderer projections are a closed vocabulary, but a future or malformed
  // payload must degrade to fixed copy rather than reaching a dynamic class or
  // throwing while conversation history renders.
  return {
    icon: "?",
    title: tool.label,
    badge: "Status unavailable",
    label: "Status unavailable",
    tone: "unknown" as const,
  };
}
