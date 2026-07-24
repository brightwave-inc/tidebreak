// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { useChatListStore, type ChatListStore } from "./ChatListStore";
import { Sidebar, type SidebarProps } from "./Sidebar";
import { useUiStore } from "./UiStore";

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
  useUiStore.setState({ primaryView: "chat", settingsPanel: null });
}

function renderSidebar(overrides: Partial<SidebarProps> = {}) {
  const props: SidebarProps = {
    nativeHost: false,
    themeMode: "light",
    updateReady: false,
    updateVersion: null,
    onCycleTheme: vi.fn(),
    onNewChat: vi.fn(),
    onSelectChat: vi.fn(),
    onStartRename: vi.fn(),
    onCommitRename: vi.fn(),
    onCancelRename: vi.fn(),
    onDeleteChat: vi.fn(),
    onRestartForUpdate: vi.fn(),
    ...overrides,
  };
  render(<Sidebar {...props} />);
  return props;
}

beforeEach(() => seedStores());
afterEach(cleanup);

describe("Sidebar", () => {
  it("lists chats from the store with the active one marked", async () => {
    const user = userEvent.setup();
    const props = renderSidebar();
    const active = screen.getByRole("button", { name: "Roadmap" });
    expect(active).toHaveAttribute("aria-current", "page");

    await user.click(active);
    expect(props.onSelectChat).toHaveBeenCalledWith(chats[0]);
    expect(props.onNewChat).not.toHaveBeenCalled();
  });

  it("drives the rename flow: draft edits hit the store, commit and cancel the owner", async () => {
    const user = userEvent.setup();
    seedStores({ renamingChatId: "chat-1", renameChatDraft: "Roadmap" });
    const props = renderSidebar();
    const input = screen.getByRole("textbox", { name: "Chat title" });
    await user.type(input, "!");
    expect(useChatListStore.getState().renameChatDraft).toBe("Roadmap!");

    await user.keyboard("{Enter}");
    expect(props.onCommitRename).toHaveBeenCalledWith(chats[0]);

    input.focus();
    await user.keyboard("{Escape}");
    expect(props.onCancelRename).toHaveBeenCalled();
  });

  it("disables chat interaction while a mutation is in flight", () => {
    seedStores({ deletingChatId: "chat-2" });
    renderSidebar();
    expect(screen.getByRole("button", { name: "Roadmap" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Deleting…" })).toBeDisabled();
  });

  it("re-renders when the store's chat list changes", () => {
    renderSidebar();
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

  it("keeps chat-scoped sources and outputs out of the sidebar", () => {
    renderSidebar({ nativeHost: true });
    expect(screen.queryByText("Sources")).not.toBeInTheDocument();
    expect(screen.queryByText("Outputs")).not.toBeInTheDocument();
  });

  it("navigates through the UI store for folders and settings", async () => {
    const user = userEvent.setup();
    renderSidebar({ nativeHost: true });
    await user.click(screen.getByText("Folders"));
    expect(useUiStore.getState().primaryView).toBe("chat");
    expect(useUiStore.getState().settingsPanel).toBe("folders");

    await user.click(screen.getByText("Settings"));
    expect(useUiStore.getState().primaryView).toBe("settings");
    expect(useUiStore.getState().settingsPanel).toBeNull();
  });

  it("shows update affordances only when an update is ready", () => {
    renderSidebar();
    expect(screen.queryByText("Restart to update")).not.toBeInTheDocument();
    cleanup();

    renderSidebar({ updateReady: true, updateVersion: "1.2.3" });
    expect(screen.getByText("Restart to update")).toBeInTheDocument();
    expect(screen.getByText("v1.2.3")).toBeInTheDocument();
  });
});
