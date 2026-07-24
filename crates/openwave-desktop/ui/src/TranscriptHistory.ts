import type { ChatMessage, ChatToolActivity } from "./api";
import type { AssistantSource } from "./AssistantSources";
import type { ToolCallStatus } from "./ToolCallCard";

export type HydratedTranscriptEntry =
  | {
      id: string;
      kind: "message";
      role: ChatMessage["role"];
      text: string;
      sources: AssistantSource[];
      createdAt: string;
    }
  | {
      id: string;
      kind: "tool";
      name: string;
      status: ToolCallStatus;
      createdAt: string;
    };

// The history endpoint exposes titles rather than canonical tool names. Keep
// the renderer on an explicit inverse allowlist so a server-side title change
// cannot become a display path for provider names, arguments, or output.
const HISTORY_TOOL_NAMES: Record<ChatToolActivity["title"], string> = {
  "Search sources": "search",
  "Check sources": "list_sources",
  "Read a source": "read_source",
  "Search the web": "web_search",
  "Read a delegated file": "read_delegated_file",
  "Read a file": "read_file",
  "Browse files": "list_dir",
  "Update a file": "write_file",
  "Create an output": "create_deliverable",
  "Request folder access": "request_folder_access",
  "Connect a folder": "connect_folder",
  "Check connected folders": "list_connected_folders",
  "Ask a question": "ask_user_questions",
  "Delegate a task": "spawn_sandbox_agent",
  "Wait for background agents": "wait_for_agents",
  "Use a tool": "historical_unknown_tool",
};

/** Build a stable, presentation-only transcript from one durable snapshot. */
export function hydrateTranscriptHistory(
  messages: ChatMessage[],
  toolActivity: ChatToolActivity[],
): HydratedTranscriptEntry[] {
  const entries: HydratedTranscriptEntry[] = [
    ...messages.map((message) => ({
      id: message.id,
      kind: "message" as const,
      role: message.role,
      text: message.content,
      // Ownership is already established by the server, which groups citations
      // by message before nesting them in each snapshot — which is why the
      // wire shape carries no `message_id`. Re-filtering on one here compared
      // against `undefined` and silently dropped every historical citation.
      sources:
        message.role === "assistant"
          ? (message.citations ?? []).map(
              ({ id, ordinal, excerpt, heading, pages }) => ({
                id,
                ordinal,
                excerpt,
                heading,
                pages,
              }),
            )
          : [],
      createdAt: message.created_at,
    })),
    ...toolActivity.map((activity, index) => ({
      // The server deliberately withholds a canonical call id. This identity
      // is therefore local to one snapshot and only supports React rendering;
      // it is never used to resolve or replay a tool operation.
      id: `tool-history:${activity.started_at}:${index}`,
      kind: "tool" as const,
      name: HISTORY_TOOL_NAMES[activity.title],
      status: activity.status,
      createdAt: activity.started_at,
    })),
  ];

  return entries.sort(
    (left, right) => Date.parse(left.createdAt) - Date.parse(right.createdAt),
  );
}
