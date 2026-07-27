// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import { useChatAttention } from "./ChatAttention";
import { useChatListStore } from "./ChatListStore";
import { ChatsPanel } from "./ChatsPanel";
import { renderWithRouter } from "./test/router";

const chats: Chat[] = [
  { id: "chat-1", title: "Roadmap", project_id: null } as unknown as Chat,
  { id: "chat-2", title: "Release notes", project_id: null } as unknown as Chat,
];

beforeEach(() => {
  useChatListStore.setState({
    chats,
    creatingChat: false,
    deletingChatId: null,
  });
  useChatAttention.getState().clear();
});
afterEach(cleanup);

describe("ChatsPanel", () => {
  it("marks waiting chats throughout the searchable list", async () => {
    useChatAttention.getState().setChatIdsWithPendingPrompts(["chat-2"]);
    await renderWithRouter(<ChatsPanel activeChatId={null} onNewChat={vi.fn()} />);

    expect(screen.getByLabelText("Release notes needs attention")).toBeInTheDocument();
    expect(screen.queryByLabelText("Roadmap needs attention")).not.toBeInTheDocument();
  });
});
