import type { TranscriptRole } from "../generated/wire";
import type { MachineClient } from "./machine";

type MachineJsonClient = Pick<MachineClient, "getJson" | "requestJson">;

export type MobileChat = {
  id: string;
  project_id: string | null;
  title: string | null;
  model: string | null;
  created_at: string;
};

export type MobileChatMessage = {
  id: string;
  role: TranscriptRole;
  content: string;
  created_at: string;
};

export type MobileChatTranscript = {
  messages: MobileChatMessage[];
  last_event_seq: number;
};

export type MobileChatQueuedTurn = {
  id: string;
  chat_id: string;
  content: string;
  attachments: string[];
  file_attachments: string[];
  invoked_skills: string[];
  voice_input_used: boolean;
  position: number;
  created_at: string;
  updated_at: string;
};

export type MobileChatQueue = {
  queued: MobileChatQueuedTurn[];
  paused: boolean;
};

export type MobileChatTurnIdentity = {
  turnId: string;
  content: string;
};

function record(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
}

function nonEmpty(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function nullableNonEmpty(value: unknown): value is string | null {
  return value === null || nonEmpty(value);
}

function nonNegativeSafeInteger(value: unknown): value is number {
  return (
    typeof value === "number" &&
    Number.isSafeInteger(value) &&
    value >= 0
  );
}

function nonEmptyStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every(nonEmpty);
}

function transcriptRole(value: unknown): value is TranscriptRole {
  return ["user", "assistant", "system", "compaction"].includes(
    String(value),
  );
}

export function parseMobileChat(value: unknown): MobileChat | null {
  const chat = record(value);
  if (
    !chat ||
    !nonEmpty(chat.id) ||
    !(chat.project_id === null || nonEmpty(chat.project_id)) ||
    !nullableNonEmpty(chat.title) ||
    !nullableNonEmpty(chat.model) ||
    !nonEmpty(chat.created_at)
  ) {
    return null;
  }
  return {
    id: chat.id,
    project_id: chat.project_id,
    title: chat.title,
    model: chat.model,
    created_at: chat.created_at,
  };
}

export function parseMobileChatMessage(
  value: unknown,
): MobileChatMessage | null {
  const message = record(value);
  if (
    !message ||
    !nonEmpty(message.id) ||
    !transcriptRole(message.role) ||
    typeof message.content !== "string" ||
    !nonEmpty(message.created_at) ||
    !Array.isArray(message.citations)
  ) {
    return null;
  }
  return {
    id: message.id,
    role: message.role,
    content: message.content,
    created_at: message.created_at,
  };
}

export function parseMobileChatTranscript(
  value: unknown,
): MobileChatTranscript | null {
  const transcript = record(value);
  if (
    !transcript ||
    !Array.isArray(transcript.messages) ||
    !Array.isArray(transcript.tool_activity) ||
    !Array.isArray(transcript.terminal_turns) ||
    !nonNegativeSafeInteger(transcript.last_event_seq)
  ) {
    return null;
  }
  const messages = transcript.messages.map(parseMobileChatMessage);
  if (messages.some((message) => message === null)) return null;
  return {
    messages: messages as MobileChatMessage[],
    last_event_seq: transcript.last_event_seq,
  };
}

export function parseMobileChatQueuedTurn(
  value: unknown,
): MobileChatQueuedTurn | null {
  const turn = record(value);
  if (
    !turn ||
    !nonEmpty(turn.id) ||
    !nonEmpty(turn.chat_id) ||
    !nonEmpty(turn.content) ||
    !nonEmptyStringArray(turn.attachments) ||
    !nonEmptyStringArray(turn.file_attachments) ||
    !nonEmptyStringArray(turn.invoked_skills) ||
    typeof turn.voice_input_used !== "boolean" ||
    !nonNegativeSafeInteger(turn.position) ||
    !nonEmpty(turn.created_at) ||
    !nonEmpty(turn.updated_at)
  ) {
    return null;
  }
  return {
    id: turn.id,
    chat_id: turn.chat_id,
    content: turn.content,
    attachments: turn.attachments,
    file_attachments: turn.file_attachments,
    invoked_skills: turn.invoked_skills,
    voice_input_used: turn.voice_input_used,
    position: turn.position,
    created_at: turn.created_at,
    updated_at: turn.updated_at,
  };
}

export function parseMobileChatQueue(value: unknown): MobileChatQueue | null {
  const snapshot = record(value);
  if (
    !snapshot ||
    !Array.isArray(snapshot.queued) ||
    typeof snapshot.paused !== "boolean"
  ) {
    return null;
  }
  const queued = snapshot.queued.map(parseMobileChatQueuedTurn);
  if (queued.some((turn) => turn === null)) return null;
  return {
    queued: queued as MobileChatQueuedTurn[],
    paused: snapshot.paused,
  };
}

function required<T>(value: T | null, label: string): T {
  if (!value) throw new Error(`${label} response contains invalid data.`);
  return value;
}

function parseChatList(value: unknown): MobileChat[] {
  if (!Array.isArray(value)) {
    throw new Error("Chat list response is not an array.");
  }
  return value.map((item) => required(parseMobileChat(item), "Chat list"));
}

export async function listMobileChats(
  client: MachineJsonClient,
): Promise<MobileChat[]> {
  return parseChatList(await client.getJson("/chats"));
}

export async function getMobileChat(
  client: MachineJsonClient,
  chatId: string,
): Promise<MobileChat> {
  return required(
    parseMobileChat(
      await client.getJson(`/chats/${encodeURIComponent(chatId)}`),
    ),
    "Chat",
  );
}

export async function createMobileChat(
  client: MachineJsonClient,
): Promise<MobileChat> {
  return required(
    parseMobileChat(
      await client.requestJson("/chats", {
        method: "POST",
        body: {},
        expectedStatus: 201,
      }),
    ),
    "Chat",
  );
}

export async function getMobileChatTranscript(
  client: MachineJsonClient,
  chatId: string,
): Promise<MobileChatTranscript> {
  return required(
    parseMobileChatTranscript(
      await client.getJson(
        `/chats/${encodeURIComponent(chatId)}/messages`,
      ),
    ),
    "Chat transcript",
  );
}

export async function listMobileChatQueuedTurns(
  client: MachineJsonClient,
  chatId: string,
): Promise<MobileChatQueue> {
  return required(
    parseMobileChatQueue(
      await client.getJson(
        `/chats/${encodeURIComponent(chatId)}/queued`,
      ),
    ),
    "Chat queue",
  );
}

export async function postMobileChatMessage(
  client: MachineJsonClient,
  chatId: string,
  turnId: string,
  content: string,
): Promise<void> {
  const message = content.trim();
  if (!message) throw new Error("Message must not be empty.");
  await client.requestJson(
    `/chats/${encodeURIComponent(chatId)}/messages`,
    {
      method: "POST",
      body: {
        turn_id: turnId,
        content: message,
        attachments: [],
        file_attachments: [],
        invoked_skills: [],
        voice_input_used: false,
        queue: true,
      },
      expectedStatus: 202,
    },
  );
}

export function chatTurnIdentityForDraft(
  pending: MobileChatTurnIdentity | null,
  content: string,
  createTurnId: () => string,
): MobileChatTurnIdentity {
  const message = content.trim();
  if (pending?.content === message) return pending;
  return { turnId: createTurnId(), content: message };
}

export function addOptimisticMobileChatQueuedTurn(
  current: MobileChatQueue | undefined,
  chatId: string,
  identity: MobileChatTurnIdentity,
  createdAt: string,
): MobileChatQueue {
  const snapshot = current ?? { queued: [], paused: false };
  if (snapshot.queued.some((turn) => turn.id === identity.turnId)) {
    return snapshot;
  }
  const position = snapshot.queued.reduce(
    (largest, turn) => Math.max(largest, turn.position),
    -1,
  ) + 1;
  return {
    ...snapshot,
    queued: [
      ...snapshot.queued,
      {
        id: identity.turnId,
        chat_id: chatId,
        content: identity.content,
        attachments: [],
        file_attachments: [],
        invoked_skills: [],
        voice_input_used: false,
        position,
        created_at: createdAt,
        updated_at: createdAt,
      },
    ],
  };
}
