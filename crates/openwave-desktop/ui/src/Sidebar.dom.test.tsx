// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { Sidebar, type SidebarProps } from "./Sidebar";

const chats: Chat[] = [
  { id: "chat-1", title: "Roadmap", project_id: null } as unknown as Chat,
  { id: "chat-2", title: null, project_id: null } as unknown as Chat,
];

function renderSidebar(overrides: Partial<SidebarProps> = {}) {
  const props: SidebarProps = {
    chats,
    activeChatId: "chat-1",
    chatsError: null,
    primaryView: "chat",
    foldersPanelOpen: false,
    nativeHost: false,
    creatingChat: false,
    deletingChatId: null,
    renamingChatId: null,
    renameChatDraft: "",
    savingTitle: false,
    themeMode: "light",
    updateReady: false,
    updateVersion: null,
    onCycleTheme: vi.fn(),
    onNewChat: vi.fn(),
    onSelectChat: vi.fn(),
    onStartRename: vi.fn(),
    onRenameDraftChange: vi.fn(),
    onCommitRename: vi.fn(),
    onCancelRename: vi.fn(),
    onDeleteChat: vi.fn(),
    onShowDocuments: vi.fn(),
    onToggleFolders: vi.fn(),
    onShowSettings: vi.fn(),
    onRestartForUpdate: vi.fn(),
    ...overrides,
  };
  render(<Sidebar {...props} />);
  return props;
}

afterEach(cleanup);

describe("Sidebar", () => {
  it("lists chats with the active one marked and selects on click", async () => {
    const user = userEvent.setup();
    const props = renderSidebar();
    const active = screen.getByRole("button", { name: "Roadmap" });
    expect(active).toHaveAttribute("aria-current", "page");

    await user.click(active);
    expect(props.onSelectChat).toHaveBeenCalledWith(chats[0]);
    expect(props.onNewChat).not.toHaveBeenCalled();
  });

  it("drives the rename flow through commit and cancel", async () => {
    const user = userEvent.setup();
    const props = renderSidebar({
      renamingChatId: "chat-1",
      renameChatDraft: "Roadmap",
    });
    const input = screen.getByRole("textbox", { name: "Chat title" });
    await user.type(input, "!");
    expect(props.onRenameDraftChange).toHaveBeenCalledWith("Roadmap!");

    await user.keyboard("{Enter}");
    expect(props.onCommitRename).toHaveBeenCalledWith(chats[0]);

    input.focus();
    await user.keyboard("{Escape}");
    expect(props.onCancelRename).toHaveBeenCalled();
  });

  it("disables chat interaction while a mutation is in flight", () => {
    renderSidebar({ deletingChatId: "chat-2" });
    expect(screen.getByRole("button", { name: "Roadmap" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Deleting…" })).toBeDisabled();
  });

  it("shows native-host and update affordances only when available", () => {
    renderSidebar();
    expect(screen.queryByText("Sources")).not.toBeInTheDocument();
    expect(screen.queryByText("Restart to update")).not.toBeInTheDocument();
    cleanup();

    renderSidebar({
      nativeHost: true,
      updateReady: true,
      updateVersion: "1.2.3",
    });
    expect(screen.getByText("Sources")).toBeInTheDocument();
    expect(screen.getByText("Restart to update")).toBeInTheDocument();
    expect(screen.getByText("v1.2.3")).toBeInTheDocument();
  });
});
