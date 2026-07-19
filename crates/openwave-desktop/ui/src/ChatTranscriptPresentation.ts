import type { ApiClient, ChatTranscript } from "./api";
import type { ChatMessage } from "./MessageList";
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
  );
  const messageIds = new Set(
    hydrated
      .filter((entry) => entry.kind === "message")
      .map((entry) => entry.id),
  );
  const messages: ChatMessage[] = hydrated.map((entry) =>
    entry.kind === "tool"
      ? {
          id: entry.id,
          role: "tool",
          callId: entry.id,
          name: entry.name,
          status: entry.status,
        }
      : entry.role === "assistant"
        ? {
            id: entry.id,
            role: "assistant",
            text: entry.text,
            sources: entry.sources,
          }
        : {
            id: entry.id,
            role: "user",
            text: entry.text,
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
