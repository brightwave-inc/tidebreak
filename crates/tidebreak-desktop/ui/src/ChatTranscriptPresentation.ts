import type {
  ApiClient,
  ChatTerminalTurn,
  ChatToolActivity,
  ChatTranscript,
  ChatMessage as WireChatMessage,
  TurnSideEffect,
} from "./api";
import type { RendererTurnUsage } from "./generated/wire";
import type { ChatMessage } from "./MessageList";
import { TURN_CANCELLED_NOTICE } from "./MessageList";
import { hydrateTranscriptHistory } from "./TranscriptHistory";

type TranscriptClient = Pick<ApiClient, "listChatMessages">;
type RetryOptions = {
  retryDelaysMs?: readonly number[];
  wait?: (delayMs: number) => Promise<void>;
};

const TERMINAL_TRANSCRIPT_RETRY_DELAYS_MS = [100, 300] as const;

/**
 * How many turns a transcript page holds: what opening a chat reads, and what
 * each "Show earlier messages" adds. Opening a long conversation used to read
 * and render every turn it ever had.
 */
export const TRANSCRIPT_PAGE_TURNS = 40;

/**
 * How many turns a refresh after a finished turn reads. Only the newest turns
 * change when one finishes, and the page is spliced in over the ones already
 * shown, so the rest of a long conversation is not read again.
 */
export const TERMINAL_REFRESH_TURNS = 10;

/** One earlier answer to a message that was answered again. */
export type AnswerVersion = {
  /** The turn that gave this answer. */
  turnId: string;
  /** The answer as the transcript shows it: prose, tool activity, notices. */
  messages: ChatMessage[];
};

/**
 * Earlier answers, oldest first, keyed by the turn shown in their place: the
 * newest answer to the same message.
 */
export type AnswerVersions = Readonly<Record<string, readonly AnswerVersion[]>>;

/** What the latest settled turn did outside the conversation. */
export type LatestTurnSideEffects = {
  turnId: string;
  effects: readonly TurnSideEffect[];
};

export type PresentedTranscript = {
  lastEventSeq: number;
  messages: ChatMessage[];
  messageIds: Set<string>;
  /** Earlier answers on this page, keyed by the turn shown in their place. */
  answerVersions: AnswerVersions;
  /**
   * What the newest turn on this page did outside the conversation, when the
   * server read it. Only the newest page carries it.
   */
  latestSideEffects: LatestTurnSideEffects | null;
  /**
   * Token counts from the chat's most recently finished turn, for the context
   * meter. Null for a chat that has never completed one.
   */
  lastTurnUsage: RendererTurnUsage | null;
  /**
   * The cursor that reads the page before this one, or null when this page
   * reaches the start of the conversation.
   */
  earlierCursor: number | null;
  /**
   * Where the conversation goes on after this page: the cursor of the first
   * message a newer page holds, or null when this page reaches the end.
   */
  laterCursor: number | null;
  /** The first durable message on this page, where it meets the one before. */
  firstMessageId: string | null;
};

/** Convert one durable snapshot into the renderer's closed message model. */
export function presentChatTranscript(
  transcript: ChatTranscript,
): PresentedTranscript {
  const { messages, messageIds } = presentEntries(
    transcript.messages,
    transcript.tool_activity,
    transcript.terminal_turns,
  );
  const latest = transcript.terminal_turns?.at(-1);

  return {
    lastEventSeq: transcript.last_event_seq,
    messages,
    messageIds,
    answerVersions: presentAnswerVersions(transcript),
    latestSideEffects:
      latest?.side_effects !== undefined
        ? { turnId: latest.turn_id, effects: latest.side_effects }
        : null,
    // The server orders terminal turns oldest-first, so the meter wants the
    // tail. Each turn re-sends the conversation, which makes the latest turn's
    // counts the current account of the window rather than one term in a sum.
    lastTurnUsage: latest?.usage ?? null,
    earlierCursor: transcript.has_more
      ? (transcript.earlier_cursor ?? null)
      : null,
    laterCursor: transcript.later_cursor ?? null,
    firstMessageId: transcript.messages[0]?.id ?? null,
  };
}

/**
 * Earlier answers, grouped by the turn now shown in their place.
 *
 * Each version is presented exactly the way the conversation is, so paging
 * back to one reads like the answer it was. A server older than versions
 * sends none.
 */
function presentAnswerVersions(transcript: ChatTranscript): AnswerVersions {
  const versions: Record<string, AnswerVersion[]> = {};
  for (const version of transcript.answer_versions ?? []) {
    const { messages } = presentEntries(
      version.messages,
      version.tool_activity,
      [version.terminal_turn],
    );
    (versions[version.current_turn_id] ??= []).push({
      turnId: version.turn_id,
      messages,
    });
  }
  return versions;
}

/** The renderer's messages for one set of durable rows. */
function presentEntries(
  wireMessages: WireChatMessage[],
  toolActivity: ChatToolActivity[],
  terminalTurns: ChatTerminalTurn[],
): { messages: ChatMessage[]; messageIds: Set<string> } {
  const hydrated = hydrateTranscriptHistory(
    wireMessages,
    toolActivity,
    terminalTurns,
  );
  const messageIds = new Set(
    hydrated
      .filter((entry) => entry.kind === "message")
      .map((entry) => entry.id),
  );
  const messages = hydrated.flatMap((entry): ChatMessage[] => {
    if (entry.kind === "terminal_turn") {
      if (entry.status === "completed") return [];
      const partial =
        entry.text || entry.reasoning
          ? [
              {
                id: `terminal:${entry.id}:assistant`,
                role: "assistant" as const,
                turnId: entry.id,
                text: entry.text,
                sources: [],
                createdAt: entry.createdAt,
                reasoning: entry.reasoning,
              } satisfies ChatMessage,
            ]
          : [];
      const outcome =
        entry.status === "failed"
          ? ({
              id: `failure:${entry.id}`,
              role: "turn_failure",
              turnId: entry.id,
              category: entry.failureCategory ?? "unknown",
              detail: entry.failureDetail,
              model: entry.failureModel,
              invokedSkills:
                entry.invokedSkills.length > 0
                  ? entry.invokedSkills
                  : undefined,
              voiceInputUsed: entry.voiceInputUsed || undefined,
            } satisfies ChatMessage)
          : ({
              id: `cancellation:${entry.id}`,
              role: "system",
              turnId: entry.id,
              text: TURN_CANCELLED_NOTICE,
            } satisfies ChatMessage);
      return [...partial, outcome];
    }
    if (entry.kind === "change_summary") {
      return [
        {
          id: entry.id,
          role: "change_summary",
          turnId: entry.turnId,
          files: entry.files,
          createdAt: entry.createdAt,
        } satisfies ChatMessage,
      ];
    }
    if (entry.kind === "memory_proposals") {
      return [
        {
          id: entry.id,
          role: "memory_proposals",
          turnId: entry.turnId,
          records: entry.records,
          createdAt: entry.createdAt,
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
    if (entry.role === "compaction") {
      return [
        {
          id: entry.id,
          role: "compaction",
        } satisfies ChatMessage,
      ];
    }
    if (entry.role === "assistant") {
      const assistant = {
        id: entry.id,
        role: "assistant",
        turnId: entry.turnId,
        text: entry.text,
        sources: entry.sources,
        createdAt: entry.createdAt,
        reasoning: entry.reasoning,
      } satisfies ChatMessage;
      if (entry.refusal) {
        return [
          assistant,
          {
            id: `refusal:${entry.id}`,
            role: "refusal",
            category: entry.refusal.category,
            partialOutput: entry.refusal.partial_output,
            source: entry.refusal.source,
          } satisfies ChatMessage,
        ];
      }
      // A cancelled turn that committed its partial prose renders as an
      // ordinary assistant message, but the stop the user asked for still
      // gets the same notice a message-less cancellation shows.
      if (entry.interrupted) {
        return [
          assistant,
          {
            id: `cancellation:${entry.id}`,
            role: "system",
            turnId: entry.turnId,
            text: TURN_CANCELLED_NOTICE,
          } satisfies ChatMessage,
        ];
      }
      return [assistant];
    }
    return [
      {
        id: entry.id,
        role: "user",
        turnId: entry.turnId,
        text: entry.text,
        images: entry.images,
        files: entry.files,
        invokedSkills:
          entry.invokedSkills.length > 0 ? entry.invokedSkills : undefined,
        createdAt: entry.createdAt,
      } satisfies ChatMessage,
    ];
  });
  return { messages, messageIds };
}

/**
 * Re-fetch a terminal turn from durable state, but return nothing after the
 * caller's chat/generation fence has gone stale.
 *
 * Reads only the newest turns: the page is spliced over the transcript already
 * held, so the earlier conversation stays as it is.
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
      const transcript = await client.listChatMessages(chatId, {
        limit: TERMINAL_REFRESH_TURNS,
      });
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
