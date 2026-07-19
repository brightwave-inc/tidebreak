import { describe, expect, it, vi } from "vitest";
import type { ChatTranscript } from "./api";
import {
  loadCurrentTerminalTranscript,
  presentChatTranscript,
} from "./ChatTranscriptPresentation";

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
          message_id: "assistant-durable",
          ordinal: 1,
          excerpt: "Bounded source excerpt",
          heading: null,
          pages: [],
        },
      ],
    },
  ],
  tool_activity: [],
  last_event_seq: 12,
};

describe("terminal transcript presentation", () => {
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
        sources: [
          {
            id: "citation-private-id",
            ordinal: 1,
            excerpt: "Bounded source excerpt",
            heading: null,
            pages: [],
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

  it("does not carry nested citations across message ownership", () => {
    const presented = presentChatTranscript({
      ...transcript,
      messages: [
        {
          ...transcript.messages[0],
          citations: [
            {
              ...transcript.messages[0].citations![0],
              message_id: "a-different-message",
              excerpt: "cross-message private excerpt",
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
        sources: [],
      },
    ]);
    expect(JSON.stringify(presented?.messages)).not.toContain(
      "cross-message private excerpt",
    );
  });
});
