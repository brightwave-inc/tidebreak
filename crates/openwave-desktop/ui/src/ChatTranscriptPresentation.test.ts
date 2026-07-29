import { describe, expect, it, vi } from "vitest";
import type { ChatTranscript } from "./api";
import {
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";
import { refusalCopy } from "./MessageList";

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
          span: { start: 0, end: 22 },
          excerpt: "Bounded source excerpt",
          heading: null,
          pages: [],
          bounds: [],
        },
      ],
    },
  ],
  tool_activity: [],
  last_event_seq: 12,
};

describe("terminal transcript presentation", () => {
  it("keeps a historical spawn bound to its durable child", () => {
    const presented = presentChatTranscript({
      messages: [],
      tool_activity: [
        {
          tool: "spawn_sandbox_agent",
          result_unreadable: false,
          status: "completed",
          started_at: "2026-07-27T12:00:00Z",
          finished_at: "2026-07-27T12:00:01Z",
          background_agent_run_id: "child-run",
        },
      ],
      last_event_seq: 4,
    });

    expect(presented.messages).toEqual([
      expect.objectContaining({
        role: "tool",
        name: "spawn_sandbox_agent",
        backgroundAgentRunId: "child-run",
      }),
    ]);
  });

  it("retains an actionable web-search setup result after terminal hydration", () => {
    const presented = presentChatTranscript({
      messages: [],
      tool_activity: [
        {
          tool: "web_search",
          result: { tool: "web_search_provider_required" },
          result_unreadable: false,
          status: "failed",
          started_at: "2026-07-27T12:00:00Z",
          finished_at: "2026-07-27T12:00:01Z",
        },
      ],
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
            span: { start: 0, end: 22 },
            excerpt: "Bounded source excerpt",
            heading: null,
            pages: [],
            bounds: [],
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
      sources: [{ excerpt: "Bounded source excerpt" }],
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
              span: { start: 30, end: 52 },
              excerpt: "Second bounded excerpt",
              heading: "Heading",
              pages: [4],
              bounds: [],
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
            span: { start: 0, end: 22 },
            excerpt: "Bounded source excerpt",
            heading: null,
            pages: [],
            bounds: [],
          },
          {
            id: "citation-second",
            ordinal: 2,
            documentId: "document-2",
            span: { start: 30, end: 52 },
            excerpt: "Second bounded excerpt",
            heading: "Heading",
            pages: [4],
            bounds: [],
          },
        ],
      },
    ]);
  });

  it("carries durable user-image identity and geometry into the message list", () => {
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
        },
      ],
      tool_activity: [],
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
          refusal: { category: "cyber", partial_output: false },
        },
        {
          id: "partial-refusal",
          role: "assistant",
          content: "Visible partial",
          created_at: "2026-07-19T10:01:00Z",
          citations: [],
          refusal: { category: "general_harms", partial_output: true },
        },
      ],
      tool_activity: [],
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
