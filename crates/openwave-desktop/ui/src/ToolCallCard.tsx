import { Check, Clock, Terminal, X } from "lucide-react";
import {
  isRendererToolName,
  type ExecResultPreview,
  type RendererToolName,
  type ToolActionPreview,
} from "./api";
import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import type { ToolTone } from "./ToolStatusIcon";
import type { ApiClient } from "./api";
import { ScrollableContainer } from "./ScrollableContainer";
import { ToolCardShell } from "./ToolCardShell";
import { toolPreviewPresentation } from "./ToolPreview";
import { ChatImage } from "./TranscriptImageAttachments";

export type ToolCallStatus =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "cancelled";

type ToolCommandCardProps = {
  name: string;
  status: ToolCallStatus;
  /**
   * The tool's own view of the command, which is what the card is for.
   *
   * Narrowed to `exec`: this card has tabs and an exit code, which describe a
   * command and nothing else. A tool whose action is fully said by one line
   * belongs in the rail, not here.
   */
  preview: Extract<ToolActionPreview, { tool: "exec" }>;
  /** What the command produced, once it has produced anything. */
  result: ExecResultPreview | null;
  imageClient?: Pick<ApiClient, "getChatImageAttachment">;
  chatId?: string;
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
//
// Keyed by [`RendererToolName`] so a tool added to that union without copy
// fails to compile instead of silently reading as "Use a tool". `other` is
// excluded: it is the server's fold for anything unrecognized, and the
// fallback below is deliberately its wording.
const TOOL_PRESENTATIONS: Record<
  Exclude<RendererToolName, "other">,
  ToolPresentation
> = {
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
  read_tool_result: {
    label: "Re-read a tool result",
    active: "Re-reading a tool result",
    complete: "Tool result read complete",
    settled: "Re-read a tool result",
  },
  web_search: {
    label: "Search the web",
    active: "Searching the web",
    complete: "Web search complete",
    settled: "Searched the web",
  },
  web_extract: {
    label: "Read a web page",
    active: "Reading a web page",
    complete: "Web page read complete",
    settled: "Read a web page",
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
  write_output_to_connected_folder: {
    label: "Publish an output",
    active: "Publishing an output",
    complete: "Output published",
    settled: "Published an output",
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
  list_folder: {
    label: "Browse a connected folder",
    active: "Browsing a connected folder",
    complete: "Connected folder browsed",
    settled: "Browsed a connected folder",
  },
  read_connected_file: {
    label: "Read a connected file",
    active: "Reading a connected file",
    complete: "Connected file read complete",
    settled: "Read a connected file",
  },
  import_connected_file: {
    label: "Add a file as a source",
    active: "Adding a file as a source",
    complete: "File added as a source",
    settled: "Added a file as a source",
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
 * running and when a completed command has visual previews to review; otherwise
 * it collapses back to one line once the badge carries the outcome.
 *
 * Only tools that project a preview get a card. Everything else lives in the
 * activity rail, where a line of text is the whole story the renderer has.
 */
export function ToolCommandCard({
  name,
  status,
  preview,
  result,
  imageClient,
  chatId,
}: ToolCommandCardProps) {
  const presentation = toolCallPresentation(name, status);
  const command = toolPreviewPresentation(preview, result);
  const running = presentation.tone === "running";
  const output = commandOutput(result);
  const images = result?.images ?? [];
  // A command that finished silently has nothing to tab between, and a
  // "Command / Output → no output" pair reads as confusing noise.
  const tabbed = running || output !== null || images.length > 0;

  return (
    <ToolCardShell
      label={`${presentation.label}: ${presentation.statusLabel}`}
      icon={<Terminal className="size-3.5 shrink-0" aria-hidden="true" />}
      title={command.headline}
      titleClassName="font-mono"
      badge={<ToolStatusBadge presentation={presentation} result={result} />}
      defaultExpanded={running || images.length > 0}
    >
      {tabbed ? (
        <Tabs defaultValue="output">
          <TabsList className="flex w-full items-center justify-start gap-1 border-b px-1">
            <TabsTrigger value="command" className="py-1 text-xs capitalize">
              command
            </TabsTrigger>
            <TabsTrigger value="output" className="py-1 text-xs capitalize">
              output
            </TabsTrigger>
          </TabsList>
          <div className="p-1">
            <TabsContent value="command" className="mt-0">
              <ScrollableContainer className="bg-muted text-muted-foreground rounded-md p-2 text-xs whitespace-pre-wrap">
                {command.detail}
              </ScrollableContainer>
            </TabsContent>
            <TabsContent value="output" className="mt-0">
              {output === null ? (
                <p className="text-muted-foreground flex items-center gap-1.5 p-2 text-xs">
                  <Spinner className="size-3.5" aria-hidden="true" />
                  Waiting for output…
                </p>
              ) : null}
              {output !== null && (
                <ScrollableContainer className="bg-muted text-muted-foreground rounded-md p-2 text-xs whitespace-pre-wrap">
                  {output}
                </ScrollableContainer>
              )}
              {images.length > 0 && imageClient && chatId && (
                <div
                  className="message-image-grid mt-2"
                  aria-label="Command preview images"
                >
                  {images.map((image, index) => (
                    <ChatImage
                      key={image.attachmentId}
                      client={imageClient}
                      chatId={chatId}
                      attachmentId={image.attachmentId}
                      mediaType={image.mediaType}
                      width={image.width}
                      height={image.height}
                      label={`Command preview ${index + 1}: ${image.width} by ${image.height} pixels`}
                      unavailableLabel="Command preview unavailable"
                    />
                  ))}
                </div>
              )}
            </TabsContent>
          </div>
        </Tabs>
      ) : (
        <div className="p-1">
          <ScrollableContainer className="bg-muted text-muted-foreground rounded-md p-2 text-xs whitespace-pre-wrap">
            {command.detail}
          </ScrollableContainer>
        </div>
      )}
    </ToolCardShell>
  );
}

/**
 * The captured streams, labelled and in the order they matter.
 *
 * `null` means nothing was captured — which for a finished command is a fact
 * worth stating by omission rather than by an empty pane.
 */
export function commandOutput(result: ExecResultPreview | null): string | null {
  if (!result) return null;
  // Streams almost always end in a newline of their own; joining those with a
  // blank line would open a three-line gap between the two sections.
  const sections = [
    result.stdout && `$ stdout\n${result.stdout.replace(/\s+$/, "")}`,
    result.stderr && `$ stderr\n${result.stderr.replace(/\s+$/, "")}`,
  ].filter((section): section is string => Boolean(section));
  if (sections.length === 0) return null;
  const body = sections.join("\n\n");
  return result.outputTruncated
    ? `${body}\n\n# output was truncated at the capture limit`
    : body;
}

function ToolStatusBadge({
  presentation,
  result,
}: {
  presentation: ToolCallPresentation;
  result: ExecResultPreview | null;
}) {
  if (presentation.tone === "running") {
    return (
      <Badge variant="outline" className="shrink-0 gap-1">
        <Spinner className="size-3" aria-hidden="true" />
        Running…
      </Badge>
    );
  }
  if (result?.timedOut) {
    return (
      <Badge variant="warning" className="shrink-0">
        Timed out
      </Badge>
    );
  }
  // A non-zero exit is the most specific thing anyone can be told about a
  // failed command, so it outranks the generic outcome word.
  if (typeof result?.exitCode === "number" && result.exitCode !== 0) {
    return (
      <Badge variant="outline" className="text-destructive shrink-0 gap-1">
        <X className="size-3" aria-hidden="true" />
        Exit {result.exitCode}
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
  const tool =
    isRendererToolName(name) && name !== "other"
      ? TOOL_PRESENTATIONS[name]
      : FALLBACK_TOOL;
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
  if (kind === "web_extract_may_fetch_url") {
    return {
      summary:
        "Allow OpenWave to fetch this exact page from the public web? The URL is shared with the page's site or the configured provider.",
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
  if (kind === "workspace_may_modify_files") {
    return {
      summary:
        "Allow OpenWave to create or modify files in this chat's workspace?",
      canApprove: true,
      // The standing "yes" for workspace edits is the chat's Auto permission
      // mode, not a per-tool grant.
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
