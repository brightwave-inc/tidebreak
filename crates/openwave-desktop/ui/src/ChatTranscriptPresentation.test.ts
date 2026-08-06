import { describe, expect, it, vi } from "vitest";
import type { ChatTranscript } from "./api";
import {
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";
import { refusalCopy, TURN_CANCELLED_NOTICE } from "./MessageList";

/** Token counts for a fixture whose subject is not the counts themselves. */
const NO_USAGE = {
  input_tokens: 0,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
};

const transcript: ChatTranscript = {
  messages: [
    {
      id: "assistant-durable",
      role: "assistant",
      content: "Clean durable answer",
      created_at: "2026-07-19T10:00:00Z",
      citations: [
        {
          id: "citation-private-id",
          ordinal: 1,
          document_id: "document-1",
          locator: { kind: "document" },
        },
      ],
    },
  ],
  tool_activity: [],
  terminal_turns: [],
  last_event_seq: 12,
};

describe("terminal transcript presentation", () => {
  // A durable host note — "User restored output …" — renders as the subtle
  // inline system notice, never as a user or assistant bubble.
  it("presents a host-authored system note as an inline notice", () => {
    const presented = presentChatTranscript({
      messages: [
        {
          id: "host-note-1",
          role: "system",
          content: "User restored output 'report.md' to the content of version 1.",
          created_at: "2026-07-30T12:00:00Z",
          citations: [],
        },
      ],
      tool_activity: [],
      terminal_turns: [],
      last_event_seq: 2,
    });

    expect(presented.messages).toEqual([
      {
        id: "host-note-1",
        role: "system",
        text: "User restored output 'report.md' to the content of version 1.",
      },
    ]);
  });

  it("keeps a historical spawn bound to its durable child", () => {
    const presented = presentChatTranscript({
      messages: [],
      tool_activity: [
        {
          call_id: "call-1",
          tool: "spawn_sandbox_agent",
          result_unreadable: false,
          status: "completed",
          started_at: "2026-07-27T12:00:00Z",
          finished_at: "2026-07-27T12:00:01Z",
          background_agent_run_id: "child-run",
        },
      ],
      terminal_turns: [],
      last_event_seq: 4,
    });

    expect(presented.messages).toEqual([
      expect.objectContaining({
        role: "tool",
        name: "spawn_sandbox_agent",
        backgroundAgentRunId: "child-run",
        // The canonical id, not an invented one: an MCP App card resolves
        // its payload by this id after rehydration.
        callId: "call-1",
      }),
    ]);
  });

  it("retains an actionable web-search setup result after terminal hydration", () => {
    const presented = presentChatTranscript({
      messages: [],
      tool_activity: [
        {
          call_id: "call-2",
          tool: "web_search",
          result: { tool: "web_search_provider_required" },
          result_unreadable: false,
          status: "failed",
          started_at: "2026-07-27T12:00:00Z",
          finished_at: "2026-07-27T12:00:01Z",
        },
      ],
      terminal_turns: [],
      last_event_seq: 4,
    });

    expect(presented.messages).toEqual([
      expect.objectContaining({
        role: "tool",
        name: "web_search",
        status: "failed",
        result: { tool: "web_search_provider_required" },
      }),
    ]);
  });

  it("carries an unreadable projection through to the renderer", () => {
    const presented = presentChatTranscript({
      messages: [],
      tool_activity: [
        {
          call_id: "call-3",
          tool: "web_search",
          result_unreadable: true,
          status: "completed",
          started_at: "2026-07-27T12:00:00Z",
          finished_at: "2026-07-27T12:00:01Z",
        },
      ],
      terminal_turns: [],
      last_event_seq: 4,
    });

    expect(presented.messages).toEqual([
      expect.objectContaining({ role: "tool", resultUnreadable: true }),
    ]);
  });

  it("replaces optimistic text with authoritative content and sources", async () => {
    const listChatMessages = vi.fn(async () => transcript);
    const presented = await loadCurrentTerminalTranscript(
      { listChatMessages },
      "chat-current",
      () => true,
    );

    expect(listChatMessages).toHaveBeenCalledWith("chat-current");
    expect(presented?.messages).toEqual([
      {
        id: "assistant-durable",
        role: "assistant",
        text: "Clean durable answer",
        createdAt: "2026-07-19T10:00:00Z",
        sources: [
          {
            id: "citation-private-id",
            ordinal: 1,
            documentId: "document-1",
            locator: { kind: "document" },
          },
        ],
      },
    ]);
    expect(presented?.lastEventSeq).toBe(12);
  });

  it("drops a terminal response after the selected chat changes", async () => {
    let resolveRequest: ((value: ChatTranscript) => void) | undefined;
    const listChatMessages = vi.fn(
      () =>
        new Promise<ChatTranscript>((resolve) => {
          resolveRequest = resolve;
        }),
    );
    let current = true;
    const pending = loadCurrentTerminalTranscript(
      { listChatMessages },
      "chat-old",
      () => current,
    );

    current = false;
    resolveRequest?.(transcript);

    await expect(pending).resolves.toBeNull();
  });

  it("retries a transient terminal fetch and returns the durable sources", async () => {
    const listChatMessages = vi
      .fn<() => Promise<ChatTranscript>>()
      .mockRejectedValueOnce(new Error("daemon briefly unavailable"))
      .mockResolvedValueOnce(transcript);
    const wait = vi.fn(async () => undefined);

    const presented = await loadCurrentTerminalTranscript(
      { listChatMessages },
      "chat-current",
      () => true,
      { retryDelaysMs: [25], wait },
    );

    expect(listChatMessages).toHaveBeenCalledTimes(2);
    expect(wait).toHaveBeenCalledWith(25);
    expect(presented?.messages[0]).toMatchObject({
      id: "assistant-durable",
      sources: [{ locator: { kind: "document" } }],
    });
  });

  it("cancels delayed retries after unmount or another generation invalidation", async () => {
    const listChatMessages = vi.fn(async () => {
      throw new Error("daemon briefly unavailable");
    });
    let current = true;
    const wait = vi.fn(async () => {
      current = false;
    });

    const presented = await loadCurrentTerminalTranscript(
      { listChatMessages },
      "chat-old",
      () => current,
      { retryDelaysMs: [25, 50], wait },
    );

    expect(presented).toBeNull();
    expect(listChatMessages).toHaveBeenCalledTimes(1);
    expect(wait).toHaveBeenCalledTimes(1);
  });

  // Citation ownership is settled server-side: `get_chat_transcript` groups by
  // message id and nests the result, so the wire carries no `message_id` for
  // the renderer to re-check. The renderer's job is to carry through exactly
  // what it was given — an earlier client-side ownership filter compared
  // against a field that is never serialized and dropped every citation.
  it("presents every citation the server nested under the message", () => {
    const presented = presentChatTranscript({
      ...transcript,
      messages: [
        {
          ...transcript.messages[0],
          citations: [
            transcript.messages[0].citations![0],
            {
              id: "citation-second",
              ordinal: 2,
              document_id: "document-2",
              locator: { kind: "page", page: 4 },
            },
          ],
        },
      ],
    });

    expect(presented.messages).toEqual([
      {
        id: "assistant-durable",
        role: "assistant",
        text: "Clean durable answer",
        createdAt: "2026-07-19T10:00:00Z",
        sources: [
          {
            id: "citation-private-id",
            ordinal: 1,
            documentId: "document-1",
            locator: { kind: "document" },
          },
          {
            id: "citation-second",
            ordinal: 2,
            documentId: "document-2",
            locator: { kind: "page", page: 4 },
          },
        ],
      },
    ]);
  });

  it("carries durable user attachments into the message list", () => {
    const presented = presentChatTranscript({
      messages: [
        {
          id: "user-image",
          role: "user",
          content: "Describe this screenshot",
          created_at: "2026-07-19T10:00:00Z",
          citations: [],
          image_attachments: [
            {
              attachment_id: "image-opaque-id",
              media_type: "image/png",
              width: 320,
              height: 240,
            },
          ],
          file_attachments: [
            {
              document_id: "document-opaque-id",
              name: "brief.pdf",
              media_type: "application/pdf",
            },
          ],
        },
      ],
      tool_activity: [],
      terminal_turns: [],
      last_event_seq: 15,
    });

    expect(presented.messages).toEqual([
      {
        id: "user-image",
        role: "user",
        text: "Describe this screenshot",
        images: [
          {
            attachmentId: "image-opaque-id",
            mediaType: "image/png",
            width: 320,
            height: 240,
          },
        ],
        files: [
          {
            documentId: "document-opaque-id",
            name: "brief.pdf",
            mediaType: "application/pdf",
          },
        ],
        createdAt: "2026-07-19T10:00:00Z",
      },
    ]);
  });

  it("hydrates empty and partial refusals beside their durable assistant output", () => {
    const presented = presentChatTranscript({
      messages: [
        {
          id: "empty-refusal",
          role: "assistant",
          content: "",
          created_at: "2026-07-19T10:00:00Z",
          citations: [],
        },
        {
          id: "partial-refusal",
          role: "assistant",
          content: "Visible partial",
          created_at: "2026-07-19T10:01:00Z",
          citations: [],
        },
      ],
      tool_activity: [],
      terminal_turns: [
        {
          turn_id: "turn-empty-refusal",
          message_id: "empty-refusal",
          status: "completed",
          partial_content: "",
          refusal: { category: "cyber", partial_output: false },
          file_changes: [],
          usage: NO_USAGE,
          finished_at: "2026-07-19T10:00:00Z",
        },
        {
          turn_id: "turn-partial-refusal",
          message_id: "partial-refusal",
          status: "completed",
          partial_content: "",
          refusal: { category: "general_harms", partial_output: true },
          file_changes: [],
          usage: NO_USAGE,
          finished_at: "2026-07-19T10:01:00Z",
        },
      ],
      last_event_seq: 14,
    });

    expect(presented.messages).toEqual([
      {
        id: "empty-refusal",
        role: "assistant",
        text: "",
        createdAt: "2026-07-19T10:00:00Z",
        sources: [],
      },
      {
        id: "refusal:empty-refusal",
        role: "refusal",
        category: "cyber",
        partialOutput: false,
      },
      {
        id: "partial-refusal",
        role: "assistant",
        text: "Visible partial",
        createdAt: "2026-07-19T10:01:00Z",
        sources: [],
      },
      {
        id: "refusal:partial-refusal",
        role: "refusal",
        category: "general_harms",
        partialOutput: true,
      },
    ]);
    expect(presented.messageIds).toEqual(
      new Set(["empty-refusal", "partial-refusal"]),
    );
  });

  it("keeps cancelled and failed reasoning at the turn that produced it", () => {
    const presented = presentChatTranscript({
      messages: [
        {
          id: "question",
          role: "user",
          content: "Long question",
          created_at: "2026-07-19T10:00:00Z",
          citations: [],
        },
        {
          id: "later-answer",
          role: "assistant",
          content: "Answer to the follow-up",
          created_at: "2026-07-19T10:03:00Z",
          citations: [],
        },
      ],
      tool_activity: [],
      terminal_turns: [
        {
          turn_id: "turn-cancelled",
          status: "cancelled",
          partial_content: "Partial answer",
          reasoning: "Considering the first approach",
          file_changes: [],
          usage: NO_USAGE,
          finished_at: "2026-07-19T10:01:00Z",
        },
        {
          turn_id: "turn-failed",
          status: "failed",
          partial_content: "",
          reasoning: "Trying a fallback",
          failure_category: "transient",
          failure_model: { id: "gpt-5.6-sol", provider: "openai" },
          file_changes: [],
          usage: NO_USAGE,
          finished_at: "2026-07-19T10:02:00Z",
        },
      ],
      last_event_seq: 20,
    });

    expect(
      presented.messages.map((message) => [message.id, message.role]),
    ).toEqual([
      ["question", "user"],
      ["terminal:turn-cancelled:assistant", "assistant"],
      ["cancellation:turn-cancelled", "system"],
      ["terminal:turn-failed:assistant", "assistant"],
      ["failure:turn-failed", "turn_failure"],
      ["later-answer", "assistant"],
    ]);
    expect(presented.messages[1]).toMatchObject({
      text: "Partial answer",
      reasoning: "Considering the first approach",
    });
    expect(presented.messages[2]).toEqual({
      id: "cancellation:turn-cancelled",
      role: "system",
      text: TURN_CANCELLED_NOTICE,
    });
    expect(presented.messages[3]).toMatchObject({
      text: "",
      reasoning: "Trying a fallback",
    });
    expect(presented.messages[4]).toEqual({
      id: "failure:turn-failed",
      role: "turn_failure",
      category: "transient",
      model: { id: "gpt-5.6-sol", provider: "openai" },
    });
  });

  it("keeps the cancellation notice on a cancelled turn that committed its partial output", () => {
    const presented = presentChatTranscript({
      messages: [
        {
          id: "question",
          role: "user",
          content: "Long question",
          created_at: "2026-07-19T10:00:00Z",
          citations: [],
        },
        {
          id: "partial-answer",
          role: "assistant",
          content: "The answer so far",
          created_at: "2026-07-19T10:00:30Z",
          citations: [],
        },
      ],
      tool_activity: [],
      terminal_turns: [
        {
          turn_id: "turn-cancelled",
          message_id: "partial-answer",
          status: "cancelled",
          partial_content: "",
          reasoning: "Considering the first approach",
          file_changes: [],
          usage: NO_USAGE,
          finished_at: "2026-07-19T10:01:00Z",
        },
      ],
      last_event_seq: 20,
    });

    expect(
      presented.messages.map((message) => [message.id, message.role]),
    ).toEqual([
      ["question", "user"],
      ["partial-answer", "assistant"],
      ["cancellation:partial-answer", "system"],
    ]);
    expect(presented.messages[1]).toMatchObject({
      text: "The answer so far",
      reasoning: "Considering the first approach",
    });
    expect(presented.messages[2]).toMatchObject({
      text: TURN_CANCELLED_NOTICE,
    });
    expect(presented.messageIds).toEqual(new Set(["question", "partial-answer"]));
  });

  it("uses renderer-owned copy that distinguishes an incomplete refusal", () => {
    expect(refusalCopy("cyber", false)).toBe(
      "The model declined this response because it matched the cyber safety category.",
    );
    expect(refusalCopy("general_harms", true)).toBe(
      "The response above is incomplete. The model declined this response because it matched the general safety category.",
    );
    expect(refusalCopy(null, false)).toBe(
      "The model declined this response because it matched a safety policy.",
    );
  });
});
