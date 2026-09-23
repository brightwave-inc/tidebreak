// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import { useChatListStore } from "./ChatListStore";
import { useOpenChat } from "./useOpenChat";

const mocks = vi.hoisted(() => ({
  client: { getChat: vi.fn() },
  navigate: vi.fn(),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mocks.navigate,
}));

function chat(id: string, archivedAt: string | null = null): Chat {
  return {
    id,
    project_id: null,
    title: "Quarterly review",
    model: null,
    reasoning_effort: null,
    permission_mode: null,
    network_policy: { mode: "off" },
    attachment_revision: 0,
    root_attachments: [],
    memory_incognito: false,
    created_at: "2026-09-01T12:00:00Z",
    last_activity_at: "2026-09-01T12:00:00Z",
    pinned_at: null,
    archived_at: archivedAt,
    running: false,
    unread: false,
    turn_count: 2,
  };
}

afterEach(cleanup);
beforeEach(() => {
  mocks.client.getChat.mockReset();
  mocks.navigate.mockReset();
  useChatListStore.setState({
    chats: [],
    archivedChats: [],
    chatsLoaded: true,
  });
});

it("keeps the open conversation when a refresh drops it, and adopts it", async () => {
  useChatListStore.setState({ chats: [chat("open")] });
  const archived = chat("open", "2026-09-10T12:00:00Z");
  mocks.client.getChat.mockResolvedValue(archived);
  const { result } = renderHook(() => useOpenChat(mocks.client, "open"));
  expect(result.current?.id).toBe("open");

  // Archived from the command line: the next list no longer holds it.
  act(() => useChatListStore.setState({ chats: [] }));
  expect(result.current?.id).toBe("open");

  await waitFor(() =>
    expect(result.current?.archived_at).toBe(archived.archived_at),
  );
  expect(mocks.client.getChat).toHaveBeenCalledWith("open");
  expect(
    useChatListStore.getState().archivedChats.map((item) => item.id),
  ).toEqual(["open"]);
  expect(mocks.navigate).not.toHaveBeenCalled();
});

it("sends a conversation that is gone home", async () => {
  mocks.client.getChat.mockRejectedValue(new Error("not found"));
  const { result } = renderHook(() => useOpenChat(mocks.client, "gone"));
  expect(result.current).toBeNull();
  await waitFor(() =>
    expect(mocks.navigate).toHaveBeenCalledWith({ to: "/", replace: true }),
  );
});

it("waits for the list before reading a conversation by id", () => {
  useChatListStore.setState({ chatsLoaded: false });
  renderHook(() => useOpenChat(mocks.client, "early"));
  expect(mocks.client.getChat).not.toHaveBeenCalled();
});
