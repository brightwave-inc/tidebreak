// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatListStore } from "./ChatListStore";
import { useUiStore } from "./UiStore";

/**
 * The shell and the routing around it, exercised the way the app runs: boot
 * resolves a server, the root route lands on a conversation, and the sidebar
 * arranges panels by navigating.
 *
 * The conversation body is stubbed. What is under test here is the frame and
 * the URL, not the transcript.
 */

const chats = [
  { id: "chat-1", title: "Roadmap", model: null, reasoning_effort: null, project_id: null },
  { id: "chat-2", title: null, model: null, reasoning_effort: null, project_id: null },
];

const listChats = vi.fn(async () => chats);
const createChat = vi.fn(async () => chats[0]);

vi.mock("./boot", () => ({
  resolveServerInfo: vi.fn(async () => ({ baseUrl: "http://127.0.0.1:1", token: "t" })),
}));

vi.mock("./api", () => ({
  ApiClient: class {
    listModels = vi.fn(async () => ({ models: [] }));
    listProviders = vi.fn(async () => ({ providers: [] }));
    listChats = listChats;
    createChat = createChat;
    openEvents = vi.fn(() => ({ close: vi.fn() }));
  },
}));

vi.mock("./host", () => ({
  hasNativeHost: () => false,
  hasMacOverlayTitlebar: () => false,
}));

vi.mock("./updates", () => ({
  useDesktopUpdates: () => ({
    state: { status: "idle", version: null },
    check: vi.fn(),
    restart: vi.fn(),
  }),
}));

vi.mock("./ChatView", () => ({
  ChatView: () => <div data-testid="transcript">transcript</div>,
}));

vi.mock("./DocumentsView", () => ({
  DocumentsView: () => <div data-testid="sources">sources</div>,
}));

vi.mock("./DeliverablesView", () => ({
  DeliverablesView: () => <div data-testid="outputs">outputs</div>,
}));

vi.mock("./FoldersView", () => ({
  FoldersView: () => <div data-testid="folders">folders</div>,
}));

vi.mock("./SettingsView", () => ({
  SettingsView: () => <div data-testid="settings">settings</div>,
}));

vi.mock("./ChatApprovalHydration", () => ({
  loadChatApprovalHydration: vi.fn(async () => null),
}));

// The resize library lays out from real element measurements, which jsdom does
// not provide — left to itself it registers no panels and renders nothing. The
// arrangement it is handed is covered by the panelSizes tests; what matters
// here is which panels the shell composes, so the group is a plain container.
vi.mock("react-resizable-panels", () => ({
  PanelGroup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  Panel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  PanelResizeHandle: () => <div />,
}));

async function mountApp() {
  const { createHashHistory, createRouter, RouterProvider } = await import(
    "@tanstack/react-router"
  );
  const { routeTree } = await import("./router");
  const router = createRouter({
    routeTree,
    history: createHashHistory(),
    defaultPreload: false,
  });
  await router.load();
  const result = render(<RouterProvider router={router as never} />);
  return { ...result, router };
}

beforeEach(() => {
  window.location.hash = "";
  listChats.mockClear();
  listChats.mockResolvedValue(chats);
  createChat.mockClear();
  // The stores outlive a test file's renders, so a chat list left behind would
  // decide the next test's routing before its own boot ever ran.
  useChatListStore.setState({ chats: [], chatsLoaded: false, selected: null });
  useUiStore.setState({ settingsOpen: false, sidebarCollapsed: false });
});
afterEach(cleanup);

describe("app shell", () => {
  it("opens on a conversation once the chat list resolves", async () => {
    const { router } = await mountApp();

    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
    expect(await screen.findByTestId("transcript")).toBeInTheDocument();
    expect(createChat).not.toHaveBeenCalled();
  });

  it("makes a conversation when there is nothing to open", async () => {
    listChats.mockResolvedValueOnce([]);
    const { router } = await mountApp();

    await waitFor(() => expect(createChat).toHaveBeenCalledOnce());
    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
  });

  it("arranges panels beside the conversation from the sidebar", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByTestId("transcript");

    await user.click(screen.getByRole("button", { name: "Sources" }));

    expect(await screen.findByTestId("sources")).toBeInTheDocument();
    // The conversation stays mounted beside it rather than being replaced.
    expect(screen.getByTestId("transcript")).toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "sources", right: "chat" }),
    );
  });

  it("closes a panel back to the conversation alone", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByTestId("transcript");
    await user.click(screen.getByRole("button", { name: "Sources" }));
    await screen.findByTestId("sources");

    await user.click(screen.getByRole("button", { name: "Close" }));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
    expect(screen.queryByTestId("sources")).not.toBeInTheDocument();
  });

  it("restores the arrangement a deep link describes", async () => {
    window.location.hash = "#/c/chat-2?left=outputs&right=chat";
    await mountApp();

    expect(await screen.findByTestId("outputs")).toBeInTheDocument();
  });

  it("keeps the conversation mounted underneath settings", async () => {
    const user = userEvent.setup();
    await mountApp();
    await screen.findByTestId("transcript");

    await user.click(screen.getByText("Settings"));

    expect(await screen.findByTestId("settings")).toBeInTheDocument();
    // The pollers for this conversation's pending approvals and questions live
    // in the transcript; stepping into settings must not stop them.
    expect(screen.getByTestId("transcript")).toBeInTheDocument();
  });
});
