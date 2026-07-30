import type { ApiClient, ChatTranscript } from "./api";
import type { ChatMessage } from "./MessageList";
import { TURN_CANCELLED_NOTICE } from "./MessageList";
import { hydrateTranscriptHistory } from "./TranscriptHistory";

type TranscriptClient = Pick<ApiClient, "listChatMessages">;
type RetryOptions = {
  retryDelaysMs?: readonly number[];
  wait?: (delayMs: number) => Promise<void>;
};

const TERMINAL_TRANSCRIPT_RETRY_DELAYS_MS = [100, 300] as const;

export type PresentedTranscript = {
  lastEventSeq: number;
  messages: ChatMessage[];
  messageIds: Set<string>;
};

/** Convert one durable snapshot into the renderer's closed message model. */
export function presentChatTranscript(
  transcript: ChatTranscript,
): PresentedTranscript {
  const hydrated = hydrateTranscriptHistory(
    transcript.messages,
    transcript.tool_activity,
    transcript.cancellations,
  );
  const messageIds = new Set(
    hydrated
      .filter((entry) => entry.kind === "message")
      .map((entry) => entry.id),
  );
  const messages = hydrated.flatMap(
    (entry): ChatMessage[] => {
      if (entry.kind === "cancellation") {
        return [
          {
            id: entry.id,
            role: "system",
            text: TURN_CANCELLED_NOTICE,
          } satisfies ChatMessage,
        ];
      }
      if (entry.kind === "tool") {
        return [
          {
            id: entry.id,
            role: "tool",
            callId: entry.callId,
            name: entry.name,
            backgroundAgentRunId: entry.backgroundAgentRunId,
            status: entry.status,
            preview: entry.preview,
            result: entry.result,
            resultUnreadable: entry.resultUnreadable,
          } satisfies ChatMessage,
        ];
      }
      // A durable host-authored note — "User restored output 'report.md'…" —
      // written for the model between turns. Shown as the same subtle inline
      // notice a cancellation uses, never as a user or assistant bubble.
      if (entry.role === "system") {
        return [
          {
            id: entry.id,
            role: "system",
            text: entry.text,
          } satisfies ChatMessage,
        ];
      }
      if (entry.role === "assistant") {
        const assistant = {
          id: entry.id,
          role: "assistant",
          text: entry.text,
          sources: entry.sources,
          createdAt: entry.createdAt,
          reasoning: entry.reasoning,
        } satisfies ChatMessage;
        return entry.refusal
          ? [
              assistant,
              {
                id: `refusal:${entry.id}`,
                role: "refusal",
                category: entry.refusal.category,
                partialOutput: entry.refusal.partial_output,
              } satisfies ChatMessage,
            ]
          : [assistant];
      }
      return [
        {
          id: entry.id,
          role: "user",
          text: entry.text,
          images: entry.images,
          files: entry.files,
          createdAt: entry.createdAt,
        } satisfies ChatMessage,
      ];
    },
  );

  return {
    lastEventSeq: transcript.last_event_seq,
    messages,
    messageIds,
  };
}

/**
 * Re-fetch a terminal turn from durable state, but return nothing after the
 * caller's chat/generation fence has gone stale.
 */
export async function loadCurrentTerminalTranscript(
  client: TranscriptClient,
  chatId: string,
  isCurrent: () => boolean,
  options: RetryOptions = {},
): Promise<PresentedTranscript | null> {
  const retryDelaysMs =
    options.retryDelaysMs ?? TERMINAL_TRANSCRIPT_RETRY_DELAYS_MS;
  const wait = options.wait ?? waitForRetry;

  for (let attempt = 0; ; attempt += 1) {
    if (!isCurrent()) return null;
    try {
      const transcript = await client.listChatMessages(chatId);
      if (!isCurrent()) return null;
      return presentChatTranscript(transcript);
    } catch (error) {
      if (!isCurrent()) return null;
      const retryDelay = retryDelaysMs[attempt];
      if (retryDelay === undefined) throw error;
      await wait(retryDelay);
    }
  }
}

function waitForRetry(delayMs: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, delayMs));
}
