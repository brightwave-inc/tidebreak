// @vitest-environment jsdom
import { act, cleanup, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AgentNotification, ApiClient } from "./api";
import * as host from "./host";
import { useNotifications } from "./NotificationStore";
import { useRefreshSignals } from "./RefreshSignals";
import { renderWithRouter } from "./test/router";
import { useAgentNotifications } from "./useAgentNotifications";
import { toast } from "sonner";

vi.mock("./host", () => ({
  isWindowFocused: vi.fn().mockResolvedValue(true),
  presentNativeNotification: vi.fn().mockResolvedValue(true),
  requestUserAttention: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("sonner", () => ({ toast: vi.fn() }));

const existing: AgentNotification = {
  id: "notification-existing",
  kind: "agent_completed",
  title: "Existing work finished",
  context: { surface: "chat", chatId: "chat-old" },
  createdAt: "2026-08-26T18:00:00Z",
  readAt: null,
};

const fresh: AgentNotification = {
  id: "notification-fresh",
  kind: "agent_failed",
  title: "Fresh work failed",
  context: { surface: "code", sessionId: "session-1", workspaceId: "ws-1" },
  createdAt: "2026-08-26T18:05:00Z",
  readAt: null,
};

function stubClient(overrides: Record<string, unknown> = {}): ApiClient {
  return {
    listNotifications: vi.fn().mockResolvedValue({
      notifications: [],
      nextCursor: null,
    }),
    notificationUnreadCount: vi.fn().mockResolvedValue(0),
    markNotificationsRead: vi.fn().mockResolvedValue(0),
    ...overrides,
  } as unknown as ApiClient;
}

function Watcher({ client }: { client: ApiClient }) {
  useAgentNotifications(client);
  return null;
}

beforeEach(() => {
  useNotifications.getState().clear();
  window.localStorage.clear();
  vi.mocked(toast).mockClear();
  vi.mocked(host.isWindowFocused).mockClear();
  vi.mocked(host.requestUserAttention).mockClear();
});

afterEach(cleanup);

describe("useAgentNotifications", () => {
  it("loads existing unread rows without replaying them as fresh alerts", async () => {
    const client = stubClient({
      listNotifications: vi.fn().mockResolvedValue({
        notifications: [existing],
        nextCursor: null,
      }),
      notificationUnreadCount: vi.fn().mockResolvedValue(1),
    });

    await renderWithRouter(<Watcher client={client} />, {
      initialUrl: "/c/chat-current",
    });
    await waitFor(() => expect(client.listNotifications).toHaveBeenCalled());

    expect(toast).not.toHaveBeenCalled();
    expect(host.requestUserAttention).not.toHaveBeenCalled();
    expect(useNotifications.getState().unread).toBe(1);
  });

  it("presents one row that appears after the initial read", async () => {
    const listNotifications = vi
      .fn()
      .mockResolvedValueOnce({ notifications: [existing], nextCursor: null })
      .mockResolvedValue({
        notifications: [fresh, existing],
        nextCursor: null,
      });
    const client = stubClient({
      listNotifications,
      notificationUnreadCount: vi.fn().mockResolvedValue(2),
    });

    await renderWithRouter(<Watcher client={client} />, {
      initialUrl: "/c/chat-current",
    });
    await waitFor(() => expect(listNotifications).toHaveBeenCalledTimes(1));

    act(() => useRefreshSignals.getState().signal("notifications"));

    await waitFor(() => expect(listNotifications).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(toast).toHaveBeenCalledTimes(1));
    expect(toast).toHaveBeenCalledWith(
      "Fresh work failed",
      expect.objectContaining({ action: expect.any(Object) }),
    );
  });

  it("marks an unread row when its conversation is open", async () => {
    const markNotificationsRead = vi.fn().mockResolvedValue(1);
    const client = stubClient({
      listNotifications: vi.fn().mockResolvedValue({
        notifications: [existing],
        nextCursor: null,
      }),
      notificationUnreadCount: vi.fn().mockResolvedValue(1),
      markNotificationsRead,
    });

    await renderWithRouter(<Watcher client={client} />, {
      initialUrl: "/c/chat-old",
    });

    await waitFor(() =>
      expect(markNotificationsRead).toHaveBeenCalledWith([
        "notification-existing",
      ]),
    );
    await waitFor(() => expect(useNotifications.getState().unread).toBe(0));
  });
});
