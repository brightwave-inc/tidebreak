// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import { useChatAttention } from "./ChatAttention";
import { useChatListStore } from "./ChatListStore";
import { ChatExplorer } from "./ChatExplorer";
import { renderWithRouter } from "./test/router";

const chats: Chat[] = [
  {
    id: "chat-1",
    title: "Roadmap",
    project_id: null,
    created_at: "2026-07-28T12:00:00Z",
  } as unknown as Chat,
  {
    id: "chat-2",
    title: "Release notes",
    project_id: null,
    created_at: "2026-07-27T12:00:00Z",
  } as unknown as Chat,
];

// jsdom reports zero for every measurement, and a virtualised grid with a
// zero-height viewport renders no rows at all. Give it a fixed box to lay out
// in so the rows under test actually exist.
beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    width: 800,
    height: 600,
    top: 0,
    left: 0,
    right: 800,
    bottom: 600,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  } as DOMRect);
  useChatListStore.setState({
    chats,
    creatingChat: false,
    deletingChatId: null,
  });
  useChatAttention.getState().clear();
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ChatExplorer", () => {
  it("marks waiting chats throughout the searchable table", async () => {
    useChatAttention.getState().setChatIdsWithPendingPrompts(["chat-2"]);
    await renderWithRouter(<ChatExplorer />);

    expect(
      await screen.findByLabelText("Release notes needs attention"),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Roadmap needs attention")).not.toBeInTheDocument();
  });

  it("filters rows by title, and says so when nothing matches", async () => {
    const user = userEvent.setup();
    await renderWithRouter(<ChatExplorer />);

    await user.type(screen.getByRole("searchbox", { name: "Search chats" }), "roadmap");
    await waitFor(() =>
      expect(screen.queryByText("Release notes")).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Roadmap")).toBeInTheDocument();

    await user.clear(screen.getByRole("searchbox", { name: "Search chats" }));
    await user.type(
      screen.getByRole("searchbox", { name: "Search chats" }),
      "nothing matches this",
    );
    expect(await screen.findByText("No matches")).toBeInTheDocument();
  });
});
