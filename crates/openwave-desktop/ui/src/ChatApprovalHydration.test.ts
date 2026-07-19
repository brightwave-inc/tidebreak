import { describe, expect, it, vi } from "vitest";
import { loadChatApprovalHydration } from "./ChatApprovalHydration";

const transcript = { messages: [], tool_activity: [], last_event_seq: 7 };

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
    const loading = loadChatApprovalHydration(client, "old-chat", () => current);
    await vi.waitFor(() => expect(client.listPendingApprovals).toHaveBeenCalled());
    current = false;
    release([]);
    await expect(loading).resolves.toBeNull();
  });
});
