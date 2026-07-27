// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useChatListStore } from "./ChatListStore";
import { Sidebar, type SidebarProps } from "./Sidebar";
import { renderWithRouter } from "./test/router";
import { createUiStore, useUiStore } from "./UiStore";

async function renderSidebar() {
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
  };
  await renderWithRouter(<Sidebar {...props} />);
}

beforeEach(() => {
  window.localStorage.clear();
  useChatListStore.setState({ chats: [], selected: null });
  useUiStore.setState({ sidebarCollapsed: false });
});
afterEach(cleanup);

describe("sidebar collapse", () => {
  it("collapses from the sidebar control and persists the preference", async () => {
    const user = userEvent.setup();
    await renderSidebar();
    await user.click(screen.getByRole("button", { name: "Hide sidebar" }));
    expect(useUiStore.getState().sidebarCollapsed).toBe(true);
    expect(window.localStorage.getItem("openwave.sidebar-collapsed")).toBe(
      "true",
    );
  });

  it("reads the stored preference at store creation", () => {
    window.localStorage.setItem("openwave.sidebar-collapsed", "true");
    const fresh = createUiStore();
    expect(fresh.getState().sidebarCollapsed).toBe(true);
  });
});
