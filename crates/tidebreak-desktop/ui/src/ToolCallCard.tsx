import { Check, Clock, Terminal, X } from "lucide-react";
import {
  isRendererToolName,
  type ExecBackend,
  type ExecDegradation,
  type ExecResultPreview,
  type RendererToolName,
  type ToolActionPreview,
} from "./api";
import { Badge } from "@/components/ui/badge";
import { Spinner } from "@/components/ui/spinner";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { ToolStatusIcon, type ToolTone } from "./ToolStatusIcon";
import { ToolCardShell } from "./ToolCardShell";
import { ToolOutputPreview } from "./ToolOutputPreview";
import { toolPreviewHeadline, toolPreviewPresentation } from "./ToolPreview";
import { useChatSessionStore } from "./ChatSessionStore";

/**
 * What a first-run sandbox image pull is doing, said where the person is
 * already looking. Registering the image pulls several gigabytes into the
 * account and returns nothing until it finishes, so a command card with no
 * output is otherwise indistinguishable from a hang.
 */
const SANDBOX_PREPARING_NOTICE =
  "Preparing the sandbox image (first run only). This downloads several gigabytes and can take a few minutes; the command runs as soon as it is ready.";

/** One line per way execution can fall short of its intended setup. */
const EXEC_DEGRADATION_NOTICE: Record<ExecDegradation, string> = {
  sandbox_image_unavailable:
    "The prepared Tidebreak sandbox image was unavailable, so commands run on the backend's stock image — document tools will install their dependencies at run time, which is slower.",
};

/**
 * Where the command ran, named on the card. A closed vocabulary like the
 * degradation notices: the server reports the backend as an enum, and the
 * words shown for each are written here.
 */
const EXEC_BACKEND_LABEL: Record<ExecBackend, string> = {
  local: "Local",
  e2b: "E2B",
  daytona: "Daytona",
  docker: "Docker",
};

export type ToolCallStatus =
  | "running"
  | "waiting_approval"
  | "completed"
  | "failed"
  | "denied"
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
  list_documents: {
    label: "Check files",
    active: "Checking files",
    complete: "Files checked",
    settled: "Checked files",
  },
  read_document: {
    label: "Read a file",
    active: "Reading a file",
    complete: "File read complete",
    settled: "Read a file",
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
  exit_plan_mode: {
    label: "Propose a plan",
    active: "Waiting for your decision",
    complete: "Plan decided",
    settled: "Proposed a plan",
  },
  update_task_plan: {
    label: "Update the task plan",
    active: "Updating the task plan",
    complete: "Task plan updated",
    settled: "Updated the task plan",
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
  create_app: {
    label: "Create an app",
    active: "Creating an app",
    complete: "App created",
    settled: "Created an app",
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
 * The title is the agent's own sentence about what it is doing, because "Ran a
 * command" is the same sentence whether it listed a directory or rebuilt the
 * workspace, and an argument vector is not a sentence at all to a reader who
 * does not read shell. A call that narrated nothing falls back to the command
 * itself, in monospace. Either way the literal command and its output are in
 * the body, one click away. It opens while the command is still running;
 * otherwise it collapses back to one line once the badge carries the
 * outcome.
 *
 * Only tools that project a preview get a card. Everything else lives in the
 * activity rail, where a line of text is the whole story the renderer has.
 */
export function ToolCommandCard({
  name,
  status,
  preview,
  result,
}: ToolCommandCardProps) {
  const presentation = toolCallPresentation(name, status);
  const command = toolPreviewPresentation(preview, result);
  const headline = toolPreviewHeadline(preview);
  const running = presentation.tone === "running";
  // Live, unjournaled state: only a command that is still running can be the
  // one waiting on the image, and a settled card must never claim it is.
  const preparing =
    useChatSessionStore((session) => session.sandboxPreparing) && running;
  const output = commandOutput(result);
  // A command that finished silently has nothing to tab between, and a
  // "Command / Output → no output" pair reads as confusing noise.
  const tabbed = running || output !== null;

  return (
    <div className="flex max-w-prose flex-col gap-1.5">
      <ToolCardShell
        label={`${presentation.label}: ${presentation.statusLabel}`}
        icon={<Terminal className="size-3.5 shrink-0" aria-hidden="true" />}
        title={headline.text}
        titleClassName={headline.literal ? "font-mono" : undefined}
        badge={
          <>
            {result?.backend && (
              <Badge
                variant="outline"
                className="text-muted-foreground shrink-0"
                title={`Ran on the ${EXEC_BACKEND_LABEL[result.backend]} execution backend`}
              >
                {EXEC_BACKEND_LABEL[result.backend]}
              </Badge>
            )}
            <ToolStatusBadge
              presentation={presentation}
              result={result}
              preparing={preparing}
            />
          </>
        }
        trailing={
          <ToolStatusIcon tone={presentation.tone} className="size-3.5" />
        }
        defaultExpanded={running || presentation.tone === "failed"}
      >
        {tabbed ? (
          <Tabs defaultValue="output">
            <TabsList className="flex w-full items-center justify-start gap-1 px-0">
              <TabsTrigger value="command" className="py-1 text-xs capitalize">
                command
              </TabsTrigger>
              <TabsTrigger value="output" className="py-1 text-xs capitalize">
                output
              </TabsTrigger>
            </TabsList>
            <div className="pt-1">
              <TabsContent value="command" className="mt-0">
                <ToolOutputPreview
                  text={command.detail}
                  collapsedLines={12}
                  label="Command"
                  bare
                />
              </TabsContent>
              <TabsContent value="output" className="mt-0">
                {output === null ? (
                  <p className="text-muted-foreground flex items-center gap-1.5 py-1 text-xs">
                    <Spinner className="size-3.5" aria-hidden="true" />
                    Waiting for output…
                  </p>
                ) : null}
                {output !== null && (
                  <ToolOutputPreview
                    text={output}
                    collapsedLines={12}
                    bare
                  />
                )}
              </TabsContent>
            </div>
          </Tabs>
        ) : (
          <ToolOutputPreview
            text={command.detail}
            collapsedLines={12}
            label="Command"
            bare
          />
        )}
      </ToolCardShell>
      {/* Outside the collapsible body on purpose: a card that reports a
          degraded run only when someone expands it has not reported it. */}
      {preparing && (
        <p
          className="text-muted-foreground flex items-start gap-1.5 text-xs"
          role="status"
        >
          <Spinner className="mt-0.5 size-3 shrink-0" aria-hidden="true" />
          {SANDBOX_PREPARING_NOTICE}
        </p>
      )}
      {result?.degraded && (
        <p className="text-muted-foreground text-xs" role="status">
          {EXEC_DEGRADATION_NOTICE[result.degraded]}
        </p>
      )}
    </div>
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
  preparing = false,
}: {
  presentation: ToolCallPresentation;
  result: ExecResultPreview | null;
  preparing?: boolean;
}) {
  if (presentation.tone === "running") {
    return (
      <Badge variant="outline" className="shrink-0 gap-1">
        <Spinner className="size-3" aria-hidden="true" />
        {preparing ? "Preparing sandbox…" : "Running…"}
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
        "Allow search to send your query and potentially matching document excerpts to configured AI services outside Tidebreak?",
      canApprove: true,
      canRemember: true,
    };
  }
  if (kind === "web_search_may_share_query") {
    return {
      summary:
        "Allow web search to send this query and its explicit filters to the configured search provider outside Tidebreak?",
      canApprove: true,
      canRemember: true,
    };
  }
  if (kind === "web_extract_may_fetch_url") {
    return {
      summary:
        "Allow Tidebreak to fetch this exact page from the public web? The URL is shared with the page's site or the configured provider.",
      canApprove: true,
      canRemember: true,
    };
  }
  if (kind === "exec_may_run_networked_command") {
    return {
      summary:
        "Allow Tidebreak to run a command that leaves this work's workspace and may reach the network?",
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
        "Allow Tidebreak to create or modify files in this work's workspace?",
      canApprove: true,
      // The standing "yes" for workspace edits is the chat's Auto permission
      // mode, not a per-tool grant.
      canRemember: false,
    };
  }
  if (kind === "delegate_may_run_background_agent") {
    return {
      summary:
        "Allow a background agent to work on this on its own, reaching the network under this work's policy?",
      canApprove: true,
      // Consent is for the whole run, so it is worth remembering: a run's own
      // calls never come back to be asked about individually.
      canRemember: true,
    };
  }
  if (kind === "computer_may_control_app") {
    return {
      summary:
        "Allow Tidebreak to operate this app — click, type, and press keys in its windows? It will capture and read the app's contents first.",
      canApprove: true,
      // The durable consent is the host broker's per-app grant, not a
      // name-keyed standing grant, so there is no renderer "always" to offer.
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
    case "denied":
      // The reader said no; the tool did not break. Distinct copy keeps a
      // decline from reading as either a crash or a cancelled turn.
      return {
        icon: "–",
        title: tool.label,
        badge: "Declined",
        label: "Declined",
        tone: "cancelled" as const,
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
