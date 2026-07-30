import { parseToolActionPreview, parseToolResultPreview } from "./api";
import type {
  ChatCancellation,
  ChatMessage,
  ChatToolActivity,
  RendererRefusal,
  ToolActionPreview,
  ToolResultPreview,
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
      sources: AssistantSource[];
      createdAt: string;
      refusal?: RendererRefusal;
      /** The turn's presentable reasoning summary, on assistant entries. */
      reasoning?: string;
    }
  | {
      id: string;
      kind: "tool";
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
      kind: "cancellation";
      createdAt: string;
    };

/** Build a stable, presentation-only transcript from one durable snapshot. */
export function hydrateTranscriptHistory(
  messages: ChatMessage[],
  toolActivity: ChatToolActivity[],
  cancellations: ChatCancellation[] = [],
): HydratedTranscriptEntry[] {
  const entries: HydratedTranscriptEntry[] = [
    ...messages.map((message) => ({
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
      // Ownership is already established by the server, which groups citations
      // by message before nesting them in each snapshot — which is why the
      // wire shape carries no `message_id`. Re-filtering on one here compared
      // against `undefined` and silently dropped every historical citation.
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
      refusal: message.refusal,
      reasoning: message.role === "assistant" ? message.reasoning : undefined,
    })),
    ...toolActivity.map((activity, index) => ({
      // The server deliberately withholds a canonical call id. This identity
      // is therefore local to one snapshot and only supports React rendering;
      // it is never used to resolve or replay a tool operation.
      id: `tool-history:${activity.started_at}:${index}`,
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
    // A stopped turn leaves no assistant message behind, so the notice is its
    // own entry placed by when the turn stopped. Keyed on the turn so the same
    // snapshot hydrates to the same React identity every time.
    ...cancellations.map((cancellation) => ({
      id: `cancellation:${cancellation.turn_id}`,
      kind: "cancellation" as const,
      createdAt: cancellation.cancelled_at,
    })),
  ];

  return entries.sort(
    (left, right) => Date.parse(left.createdAt) - Date.parse(right.createdAt),
  );
}
