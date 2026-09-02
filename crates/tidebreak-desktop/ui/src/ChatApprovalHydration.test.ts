import { describe, expect, it, vi } from "vitest";
import {
  loadChatApprovalHydration,
  sessionFromOpenedChat,
} from "./ChatApprovalHydration";
import { initialChatSessionState } from "./ChatSessionReducer";

const transcript = {
  messages: [],
  tool_activity: [],
  terminal_turns: [],
  last_event_seq: 7,
};

const USAGE = {
  input_tokens: 12_000,
  output_tokens: 800,
  cache_read_input_tokens: 4_000,
  cache_creation_input_tokens: 0,
};

describe("loadChatApprovalHydration", () => {
  it("loads pending approvals after the transcript cursor boundary", async () => {
    const order: string[] = [];
    const client = {
      listChatMessages: vi.fn(async () => {
        order.push("transcript");
        return transcript;
      }),
      listPendingApprovals: vi.fn(async () => {
        order.push("approvals");
        return [];
      }),
    };
    await expect(
      loadChatApprovalHydration(client, "chat-1", () => true),
    ).resolves.toEqual({ transcript, pendingApprovals: [] });
    expect(order).toEqual(["transcript", "approvals"]);
  });

  it("drops a stale chat-switch response before renderer state changes", async () => {
    let current = true;
    let release!: (value: never[]) => void;
    const pending = new Promise<never[]>((resolve) => {
      release = resolve;
    });
    const client = {
      listChatMessages: vi.fn(async () => transcript),
      listPendingApprovals: vi.fn(() => pending),
    };
    const loading = loadChatApprovalHydration(
      client,
      "old-chat",
      () => current,
    );
    await vi.waitFor(() =>
      expect(client.listPendingApprovals).toHaveBeenCalled(),
    );
    current = false;
    release([]);
    await expect(loading).resolves.toBeNull();
  });
});

describe("sessionFromOpenedChat", () => {
  it("hydrates lastTurnUsage so a reopened chat meters without a new turn", () => {
    const opened = sessionFromOpenedChat(
      initialChatSessionState(),
      {
        messages: [],
        tool_activity: [],
        terminal_turns: [
          {
            turn_id: "turn-1",
            message_id: "assistant-1",
            status: "completed",
            partial_content: "",
            file_changes: [],
            memory_proposals: [],
            usage: USAGE,
            voice_input_used: false,
            finished_at: "2026-07-19T10:00:00Z",
          },
        ],
        last_event_seq: 12,
      },
      [],
    );

    expect(opened.lastTurnUsage).toEqual(USAGE);
    expect(opened.lastSeq).toBe(12);
    expect(opened.busy).toBe(false);
    expect(opened.activeTurnId).toBeNull();
  });
});
