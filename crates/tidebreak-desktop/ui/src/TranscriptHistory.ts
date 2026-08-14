import { parseToolActionPreview, parseToolResultPreview } from "./api";
import type {
  ChatMessage,
  ChatTerminalTurn,
  ChatToolActivity,
  RendererRefusal,
  ToolActionPreview,
  ToolResultPreview,
  ExecFileChangeSummary,
} from "./api";
import type { AssistantSource } from "./AssistantSources";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import type { TranscriptFileAttachment } from "./TranscriptFileAttachments";
import type { ToolCallStatus } from "./ToolCallCard";

export type HydratedTranscriptEntry =
  | {
      id: string;
      kind: "message";
      role: ChatMessage["role"];
      text: string;
      images: TranscriptImageAttachment[];
      files: TranscriptFileAttachment[];
      /** Skills a user message named, empty on every other role. */
      invokedSkills: readonly string[];
      sources: AssistantSource[];
      createdAt: string;
      refusal?: RendererRefusal;
      /** The turn's presentable reasoning summary, on assistant entries. */
      reasoning?: string;
      /**
       * The turn was cancelled after this assistant message's prose streamed;
       * the partial output is durable but the stop still needs its notice.
       */
      interrupted?: boolean;
    }
  | {
      id: string;
      kind: "tool";
      /** Canonical call id — the payload key for a rehydrated MCP App view. */
      callId: string;
      name: string;
      /** Opaque child correlation for a historical sandbox spawn. */
      backgroundAgentRunId?: string;
      preview: ToolActionPreview | null;
      result: ToolResultPreview | null;
      /** Set when a retained projection no longer parses against this build. */
      resultUnreadable?: boolean;
      status: ToolCallStatus;
      createdAt: string;
    }
  | {
      id: string;
      kind: "terminal_turn";
      status: ChatTerminalTurn["status"];
      text: string;
      reasoning?: string;
      failureCategory?: NonNullable<ChatTerminalTurn["failure_category"]>;
      failureDetail?: NonNullable<ChatTerminalTurn["failure_detail"]>;
      failureModel?: NonNullable<ChatTerminalTurn["failure_model"]>;
      invokedSkills: readonly string[];
      voiceInputUsed: boolean;
      createdAt: string;
    }
  | {
      id: string;
      kind: "change_summary";
      turnId: string;
      files: ExecFileChangeSummary[];
      createdAt: string;
    };

/** Build a stable, presentation-only transcript from one durable snapshot. */
export function hydrateTranscriptHistory(
  messages: ChatMessage[],
  toolActivity: ChatToolActivity[],
  terminalTurns: ChatTerminalTurn[] = [],
): HydratedTranscriptEntry[] {
  const turnByMessageId = new Map(
    terminalTurns.flatMap((turn) =>
      turn.message_id ? ([[turn.message_id, turn]] as const) : [],
    ),
  );
  const entries: HydratedTranscriptEntry[] = [
    ...messages.map((message) => {
      const terminalTurn = turnByMessageId.get(message.id);
      return {
        id: message.id,
        kind: "message" as const,
        role: message.role,
        text: message.content,
        images:
          message.role === "user"
            ? (message.image_attachments ?? []).map(
                ({ attachment_id, media_type, width, height }) => ({
                  attachmentId: attachment_id,
                  mediaType: media_type,
                  width,
                  height,
                }),
              )
            : [],
        files:
          message.role === "user"
            ? (message.file_attachments ?? []).map(
                ({ document_id, name, media_type }) => ({
                  documentId: document_id,
                  name,
                  mediaType: media_type,
                }),
              )
            : [],
        invokedSkills:
          message.role === "user" ? (message.invoked_skills ?? []) : [],
        // Ownership is already established by the server, which groups
        // citations by message before nesting them in each snapshot.
        sources:
          message.role === "assistant"
            ? (message.citations ?? []).map(
                ({ id, ordinal, document_id, locator }) => ({
                  id,
                  ordinal,
                  documentId: document_id,
                  locator,
                }),
              )
            : [],
        createdAt: message.created_at,
        refusal:
          message.role === "assistant" ? terminalTurn?.refusal : undefined,
        reasoning:
          message.role === "assistant" ? terminalTurn?.reasoning : undefined,
        interrupted:
          message.role === "assistant" &&
          terminalTurn?.status === "cancelled",
      };
    }),
    ...toolActivity.map((activity) => ({
      // The canonical call id, not a snapshot-local one: an MCP App card
      // resolves its payload by this id, so inventing an identity here left
      // every rehydrated app view fetching a payload the server rejected.
      id: activity.call_id,
      callId: activity.call_id,
      kind: "tool" as const,
      // Already an allowlisted renderer tool name, folded server-side, so the
      // same presentation the live stream uses applies directly.
      name: activity.tool,
      backgroundAgentRunId: activity.background_agent_run_id,
      status: activity.status,
      preview: parseToolActionPreview(activity.action),
      // Arbitrary result text stays server-side. A tool can retain only a
      // closed renderer result such as an actionable configuration signal.
      result: parseToolResultPreview(activity.result),
      // A projection the server retained but this build cannot read is a
      // different fact from a call that projected nothing, and the card says so
      // rather than silently showing no result at all.
      resultUnreadable: activity.result_unreadable,
      createdAt: activity.started_at,
    })),
    // Terminal turns that left no assistant message behind keep their streamed
    // content and status adjacent in transcript order, with a stable identity
    // across hydrations. A cancellation after a committed step is associated
    // with that message above instead.
    ...terminalTurns
      .filter((turn) => !turn.message_id)
      .map((turn) => ({
        id: turn.turn_id,
        kind: "terminal_turn" as const,
        status: turn.status,
        text: turn.partial_content,
        reasoning: turn.reasoning,
        failureCategory: turn.failure_category,
        failureDetail: turn.failure_detail,
        failureModel: turn.failure_model,
        invokedSkills: turn.invoked_skills ?? [],
        voiceInputUsed: turn.voice_input_used ?? false,
        createdAt: turn.finished_at,
      })),
    ...terminalTurns
      .filter((turn) => turn.file_changes.length > 0)
      .map((turn) => ({
        id: `changes:${turn.turn_id}`,
        kind: "change_summary" as const,
        turnId: turn.turn_id,
        files: turn.file_changes,
        createdAt: turn.finished_at,
      })),
  ];

  return entries.sort(
    (left, right) => Date.parse(left.createdAt) - Date.parse(right.createdAt),
  );
}
