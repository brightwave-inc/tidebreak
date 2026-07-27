import { parseToolActionPreview } from "./api";
import type {
  ChatMessage,
  ChatToolActivity,
  RendererRefusal,
  ToolActionPreview,
} from "./api";
import type { AssistantSource } from "./AssistantSources";
import type { TranscriptImageAttachment } from "./ImageAttachments";
import type { ToolCallStatus } from "./ToolCallCard";

export type HydratedTranscriptEntry =
  | {
      id: string;
      kind: "message";
      role: ChatMessage["role"];
      text: string;
      images: TranscriptImageAttachment[];
      sources: AssistantSource[];
      createdAt: string;
      refusal?: RendererRefusal;
    }
  | {
      id: string;
      kind: "tool";
      name: string;
      /** Opaque child correlation for a historical sandbox spawn. */
      backgroundAgentRunId?: string;
      preview: ToolActionPreview | null;
      status: ToolCallStatus;
      createdAt: string;
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
      refusal: message.refusal,
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
      // History carries what the call did but not what it produced: results
      // are not persisted, so a reloaded command card shows its command alone.
      preview: parseToolActionPreview(activity.action),
      createdAt: activity.started_at,
    })),
  ];

  return entries.sort(
    (left, right) => Date.parse(left.createdAt) - Date.parse(right.createdAt),
  );
}
