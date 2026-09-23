// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import { useChatListStore } from "./ChatListStore";
import { LegacyChatRoute } from "./LegacyChatRoute";

const mocks = vi.hoisted(() => ({
  client: { getCodeSession: vi.fn(), getChat: vi.fn() },
  navigate: vi.fn(),
}));
vi.mock("./AppContext", () => ({ useApp: () => ({ client: mocks.client }) }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => mocks.navigate,
}));
vi.mock("./ChatRoute", () => ({
  ChatRoute: ({ chatId }: { chatId: string }) => <p>Owner chat {chatId}</p>,
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
  mocks.client.getCodeSession.mockReset();
  mocks.client.getChat.mockReset();
  mocks.navigate.mockReset();
  useChatListStore.setState({
    chats: [],
    archivedChats: [],
    chatsLoaded: true,
  });
});

it("opens a conversation the list holds without asking the server", () => {
  useChatListStore.setState({ chats: [chat("listed")] });
  render(<LegacyChatRoute chatId="listed" />);
  expect(screen.getByText("Owner chat listed")).toBeTruthy();
  expect(mocks.client.getCodeSession).not.toHaveBeenCalled();
  expect(mocks.client.getChat).not.toHaveBeenCalled();
});

it("waits for the list before deciding a chat is absent", () => {
  useChatListStore.setState({ chatsLoaded: false });
  render(<LegacyChatRoute chatId="early" />);
  expect(screen.getByRole("status").textContent).toBe("Opening conversation…");
  expect(mocks.client.getCodeSession).not.toHaveBeenCalled();
});

it("redirects an authorized shared legacy link before owner chat mounts", async () => {
  mocks.client.getCodeSession.mockResolvedValue({
    id: "shared",
    is_owner: false,
  });
  render(<LegacyChatRoute chatId="shared" />);
  await waitFor(() =>
    expect(mocks.navigate).toHaveBeenCalledWith({
      to: "/code/s/$sessionId",
      params: { sessionId: "shared" },
      replace: true,
    }),
  );
  expect(screen.queryByText("Owner chat shared")).toBeNull();
});

it("adopts an archived conversation opened by link", async () => {
  mocks.client.getCodeSession.mockResolvedValue({
    id: "archived",
    is_owner: true,
    workspace_id: null,
    harness_kind: "internal",
  });
  mocks.client.getChat.mockResolvedValue(
    chat("archived", "2026-09-10T12:00:00Z"),
  );
  render(<LegacyChatRoute chatId="archived" />);
  expect(await screen.findByText("Owner chat archived")).toBeTruthy();
  expect(
    useChatListStore.getState().archivedChats.map((item) => item.id),
  ).toEqual(["archived"]);
  expect(mocks.navigate).not.toHaveBeenCalled();
});

it("does not turn a failed access lookup into shared access", async () => {
  mocks.client.getCodeSession.mockRejectedValue(new Error("not found"));
  mocks.client.getChat.mockRejectedValue(new Error("not found"));
  render(<LegacyChatRoute chatId="private" />);
  expect(await screen.findByText("Owner chat private")).toBeTruthy();
  expect(mocks.navigate).not.toHaveBeenCalled();
});

it.each([
  { harness_kind: "claude_code", workspace_id: null },
  { harness_kind: "codex", workspace_id: null },
  { harness_kind: "claude_code", workspace_id: "workspace" },
  { harness_kind: "internal", workspace_id: "workspace" },
])(
  "opens the owner's code-backed legacy link through the session route: %o",
  async (binding) => {
    mocks.client.getCodeSession.mockResolvedValue({
      id: "owned-code",
      is_owner: true,
      ...binding,
    });
    render(<LegacyChatRoute chatId="owned-code" />);
    await waitFor(() =>
      expect(mocks.navigate).toHaveBeenCalledWith({
        to: "/code/s/$sessionId",
        params: { sessionId: "owned-code" },
        replace: true,
      }),
    );
    expect(screen.queryByText("Owner chat owned-code")).toBeNull();
  },
);
