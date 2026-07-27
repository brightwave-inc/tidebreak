// @vitest-environment jsdom
import { act, cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { useChatListStore, type ChatListStore } from "./ChatListStore";
import { Sidebar, type SidebarProps } from "./Sidebar";
import { renderWithRouter } from "./test/router";

const chats: Chat[] = [
  { id: "chat-1", title: "Roadmap", project_id: null } as unknown as Chat,
  { id: "chat-2", title: null, project_id: null } as unknown as Chat,
];

function seedStores(overrides: Partial<ChatListStore> = {}) {
  useChatListStore.setState({
    chats,
    selected: chats[0],
    chatsError: null,
    creatingChat: false,
    deletingChatId: null,
    renamingChatId: null,
    renameChatDraft: "",
    savingTitle: false,
    ...overrides,
  });
}

async function renderSidebar(overrides: Partial<SidebarProps> = {}) {
  const props: SidebarProps = {
    themeMode: "light",
    updateReady: false,
    updateVersion: null,
    onCycleTheme: vi.fn(),
    onNewChat: vi.fn(),
    onStartRename: vi.fn(),
    onCommitRename: vi.fn(),
    onCancelRename: vi.fn(),
    onDeleteChat: vi.fn(),
    onRestartForUpdate: vi.fn(),
    ...overrides,
  };
  const { router } = await renderWithRouter(<Sidebar {...props} />);
  return { props, router };
}

beforeEach(() => seedStores());
afterEach(cleanup);

describe("Sidebar", () => {
  it("lists chats from the store with the active one marked", async () => {
    const user = userEvent.setup();
    const { props, router } = await renderSidebar();
    const active = screen.getByRole("button", { name: "Roadmap" });
    expect(active).toHaveAttribute("aria-current", "page");

    await user.click(active);
    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
    expect(props.onNewChat).not.toHaveBeenCalled();
  });

  it("drives the rename flow: draft edits hit the store, commit and cancel the owner", async () => {
    const user = userEvent.setup();
    seedStores({ renamingChatId: "chat-1", renameChatDraft: "Roadmap" });
    const { props } = await renderSidebar();
    const input = screen.getByRole("textbox", { name: "Chat title" });
    await user.type(input, "!");
    expect(useChatListStore.getState().renameChatDraft).toBe("Roadmap!");

    await user.keyboard("{Enter}");
    expect(props.onCommitRename).toHaveBeenCalledWith(chats[0]);

    input.focus();
    await user.keyboard("{Escape}");
    expect(props.onCancelRename).toHaveBeenCalled();
  });

  it("disables chat interaction while a mutation is in flight", async () => {
    seedStores({ deletingChatId: "chat-2" });
    await renderSidebar();
    expect(screen.getByRole("button", { name: "Roadmap" })).toBeDisabled();
    // An untitled conversation is also labelled "New chat", so the action has
    // to be picked out from the list rows that share its name.
    const recentList = screen.getByLabelText("Chats");
    const [startChat] = screen
      .getAllByRole("button", { name: "New chat" })
      .filter((button) => !recentList.contains(button));
    expect(startChat).toBeDisabled();
  });

  it("re-renders when the store's chat list changes", async () => {
    await renderSidebar();
    expect(screen.queryByText("Retro notes")).not.toBeInTheDocument();
    act(() => {
      useChatListStore
        .getState()
        .prependChat({
          id: "chat-3",
          title: "Retro notes",
          project_id: null,
        } as unknown as Chat);
    });
    expect(screen.getByText("Retro notes")).toBeInTheDocument();
  });

  it("opens a workspace panel by writing it into the URL", async () => {
    const user = userEvent.setup();
    const { router } = await renderSidebar();

    await user.click(screen.getByRole("button", { name: "Sources" }));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "sources", right: "chat" }),
    );

    // Opening a second navigation panel replaces the first rather than
    // stacking it, because both belong on the same side.
    await user.click(screen.getByRole("button", { name: "Outputs" }));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "outputs", right: "chat" }),
    );
  });

  it("marks the panel the workspace is showing", async () => {
    const user = userEvent.setup();
    await renderSidebar();
    const sources = screen.getByRole("button", { name: "Sources" });
    expect(sources).not.toHaveAttribute("aria-current");

    await user.click(sources);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Sources" })).toHaveAttribute(
        "aria-current",
        "page",
      ),
    );
  });

  it("navigates to settings", async () => {
    const user = userEvent.setup();
    const { router } = await renderSidebar();
    await user.click(screen.getByText("Settings"));
    await waitFor(() => expect(router.state.location.pathname).toBe("/settings"));
  });

  it("shows update affordances only when an update is ready", async () => {
    await renderSidebar();
    expect(screen.queryByText("Restart to update")).not.toBeInTheDocument();
    cleanup();

    await renderSidebar({ updateReady: true, updateVersion: "1.2.3" });
    expect(screen.getByText("Restart to update")).toBeInTheDocument();
    expect(screen.getByText("v1.2.3")).toBeInTheDocument();
  });
});
