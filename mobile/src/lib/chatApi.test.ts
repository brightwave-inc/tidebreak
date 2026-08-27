import { describe, expect, it, vi } from "vitest";
import type { MachineClient } from "./machine";
import {
  addOptimisticMobileChatQueuedTurn,
  chatTurnIdentityForDraft,
  createMobileChat,
  getMobileChat,
  getMobileChatTranscript,
  listMobileChatQueuedTurns,
  listMobileChats,
  parseMobileChat,
  parseMobileChatQueue,
  parseMobileChatQueuedTurn,
  parseMobileChatTranscript,
  postMobileChatMessage,
} from "./chatApi";

const chat = {
  id: "chat-1",
  project_id: null,
  title: "Launch review",
  model: "model-gateway::gpt-5.6-sol",
  reasoning_effort: "high",
  permission_mode: "ask",
  network_policy: { mode: "open" },
  attachment_revision: 0,
  root_attachments: [],
  created_at: "2026-08-27T20:00:00Z",
};

const transcript = {
  messages: [
    {
      id: "message-1",
      role: "user",
      content: "Review the launch flow.",
      created_at: "2026-08-27T20:00:01Z",
      citations: [],
    },
  ],
  tool_activity: [],
  terminal_turns: [],
  last_event_seq: 3,
};

const queuedTurn = {
  id: "00000000-0000-4000-8000-000000000001",
  chat_id: "chat-1",
  content: "Continue.",
  attachments: [],
  file_attachments: [],
  invoked_skills: [],
  voice_input_used: false,
  position: 0,
  created_at: "2026-08-27T20:00:02Z",
  updated_at: "2026-08-27T20:00:02Z",
};

function fakeClient(response: unknown): {
  client: Pick<MachineClient, "getJson" | "requestJson">;
  getJson: ReturnType<typeof vi.fn>;
  requestJson: ReturnType<typeof vi.fn>;
} {
  const getJson = vi.fn(async () => response);
  const requestJson = vi.fn(async () => response);
  return { client: { getJson, requestJson }, getJson, requestJson };
}

describe("mobile chat API contracts", () => {
  it("validates chat rows and rejects a partial list", async () => {
    expect(parseMobileChat(chat)).toEqual({
      id: "chat-1",
      project_id: null,
      title: "Launch review",
      model: "model-gateway::gpt-5.6-sol",
      created_at: "2026-08-27T20:00:00Z",
    });
    expect(parseMobileChat({ id: "chat-1" })).toBeNull();
    expect(parseMobileChat({ ...chat, model: "" })).toBeNull();

    const listed = fakeClient([chat]);
    await expect(listMobileChats(listed.client)).resolves.toHaveLength(1);
    expect(listed.getJson).toHaveBeenCalledWith("/chats");

    const invalid = fakeClient([chat, { ...chat, created_at: null }]);
    await expect(listMobileChats(invalid.client)).rejects.toThrow(/invalid data/);
  });

  it("loads one chat and its renderer-safe transcript", async () => {
    const one = fakeClient(chat);
    await expect(getMobileChat(one.client, "chat/1")).resolves.toMatchObject({
      id: "chat-1",
    });
    expect(one.getJson).toHaveBeenCalledWith("/chats/chat%2F1");

    expect(parseMobileChatTranscript(transcript)).toEqual({
      messages: [
        {
          id: "message-1",
          role: "user",
          content: "Review the launch flow.",
          created_at: "2026-08-27T20:00:01Z",
        },
      ],
      last_event_seq: 3,
    });
    expect(
      parseMobileChatTranscript({
        ...transcript,
        messages: [{ ...transcript.messages[0], citations: null }],
      }),
    ).toBeNull();
    expect(
      parseMobileChatTranscript({ ...transcript, last_event_seq: -1 }),
    ).toBeNull();

    const history = fakeClient(transcript);
    await expect(
      getMobileChatTranscript(history.client, "chat/1"),
    ).resolves.toMatchObject({
      messages: [expect.objectContaining({ id: "message-1" })],
      last_event_seq: 3,
    });
    expect(history.getJson).toHaveBeenCalledWith("/chats/chat%2F1/messages");
  });

  it("creates a chat with sticky defaults", async () => {
    const created = fakeClient(chat);
    await expect(createMobileChat(created.client)).resolves.toMatchObject({
      id: "chat-1",
    });
    expect(created.requestJson).toHaveBeenCalledWith("/chats", {
      method: "POST",
      body: {},
      expectedStatus: 201,
    });
  });

  it("loads and validates a chat queue", async () => {
    expect(parseMobileChatQueuedTurn(queuedTurn)).toEqual(queuedTurn);
    expect(
      parseMobileChatQueuedTurn({ ...queuedTurn, attachments: [null] }),
    ).toBeNull();
    expect(
      parseMobileChatQueuedTurn({ ...queuedTurn, position: -1 }),
    ).toBeNull();
    expect(
      parseMobileChatQueuedTurn({ ...queuedTurn, content: "" }),
    ).toBeNull();

    const snapshot = { queued: [queuedTurn], paused: true };
    expect(parseMobileChatQueue(snapshot)).toEqual(snapshot);
    expect(parseMobileChatQueue({ ...snapshot, paused: "yes" })).toBeNull();

    const listed = fakeClient(snapshot);
    await expect(
      listMobileChatQueuedTurns(listed.client, "chat/1"),
    ).resolves.toEqual(snapshot);
    expect(listed.getJson).toHaveBeenCalledWith("/chats/chat%2F1/queued");

    const invalid = fakeClient({
      queued: [queuedTurn, { ...queuedTurn, updated_at: null }],
      paused: false,
    });
    await expect(
      listMobileChatQueuedTurns(invalid.client, "chat-1"),
    ).rejects.toThrow(/invalid data/);
  });

  it("posts a queued, idempotent turn with the caller's identity", async () => {
    const sent = fakeClient(undefined);
    await postMobileChatMessage(
      sent.client,
      "chat/1",
      "00000000-0000-4000-8000-000000000001",
      "  Continue.  ",
    );
    expect(sent.requestJson).toHaveBeenCalledWith(
      "/chats/chat%2F1/messages",
      {
        method: "POST",
        body: {
          turn_id: "00000000-0000-4000-8000-000000000001",
          content: "Continue.",
          attachments: [],
          file_attachments: [],
          invoked_skills: [],
          voice_input_used: false,
          queue: true,
        },
        expectedStatus: 202,
      },
    );

    await expect(
      postMobileChatMessage(sent.client, "chat-1", "turn-2", "   "),
    ).rejects.toThrow(/must not be empty/);
  });

  it("reuses a turn id when the same failed draft is retried", () => {
    const pending = {
      turnId: "00000000-0000-4000-8000-000000000001",
      content: "Continue.",
    };
    expect(
      chatTurnIdentityForDraft(pending, "  Continue.  ", () => "new-turn"),
    ).toBe(pending);
    expect(
      chatTurnIdentityForDraft(pending, "Try another way.", () => "new-turn"),
    ).toEqual({ turnId: "new-turn", content: "Try another way." });
  });

  it("adds one optimistic queued turn and deduplicates its turn id", () => {
    const identity = {
      turnId: "00000000-0000-4000-8000-000000000002",
      content: "Try another way.",
    };
    const snapshot = addOptimisticMobileChatQueuedTurn(
      { queued: [queuedTurn], paused: true },
      "chat-1",
      identity,
      "2026-08-27T20:00:03Z",
    );
    expect(snapshot).toEqual({
      paused: true,
      queued: [
        queuedTurn,
        {
          id: identity.turnId,
          chat_id: "chat-1",
          content: identity.content,
          attachments: [],
          file_attachments: [],
          invoked_skills: [],
          voice_input_used: false,
          position: 1,
          created_at: "2026-08-27T20:00:03Z",
          updated_at: "2026-08-27T20:00:03Z",
        },
      ],
    });
    expect(
      addOptimisticMobileChatQueuedTurn(
        snapshot,
        "chat-1",
        identity,
        "2026-08-27T20:00:04Z",
      ),
    ).toBe(snapshot);
  });
});
