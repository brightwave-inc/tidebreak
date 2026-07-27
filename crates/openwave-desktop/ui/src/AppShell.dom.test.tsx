// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatListStore } from "./ChatListStore";
import { usePendingPrompts } from "./PendingPrompts";
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
const listPendingUserQuestions = vi.fn(async () => [] as unknown[]);
const listPendingFolderAccessRequests = vi.fn(async () => [] as unknown[]);
const requestUserAttention = vi.fn(async () => {});
const postMessage = vi.fn(async () => {});

vi.mock("./boot", () => ({
  resolveServerInfo: vi.fn(async () => ({ baseUrl: "http://127.0.0.1:1", token: "t" })),
}));

vi.mock("./api", () => ({
  ApiClient: class {
    listModels = vi.fn(async () => ({ models: [] }));
    listProviders = vi.fn(async () => ({ providers: [] }));
    listChats = listChats;
    createChat = createChat;
    listPendingUserQuestions = listPendingUserQuestions;
    listPendingFolderAccessRequests = listPendingFolderAccessRequests;
    postMessage = postMessage;
    openEvents = vi.fn(() => ({ close: vi.fn() }));
  },
}));

vi.mock("./host", () => ({
  hasNativeHost: () => false,
  hasMacOverlayTitlebar: () => false,
  requestUserAttention,
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
  SettingsView: ({ onBack }: { onBack: () => void }) => (
    <div data-testid="settings">
      settings
      <button onClick={onBack}>Back to app</button>
    </div>
  ),
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

async function mountApp({ at }: { at?: string } = {}) {
  if (at) window.location.hash = `#${at}`;
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
  listPendingUserQuestions.mockClear();
  listPendingUserQuestions.mockResolvedValue([]);
  listPendingFolderAccessRequests.mockClear();
  requestUserAttention.mockClear();
  postMessage.mockClear();
  usePendingPrompts.setState({ chatId: null, userQuestions: [], folderAccess: [] });
  // The stores outlive a test file's renders, so a chat list left behind would
  // decide the next test's routing before its own boot ever ran.
  useChatListStore.setState({ chats: [], chatsLoaded: false, selected: null });
  useUiStore.setState({ sidebarCollapsed: false });
});
afterEach(cleanup);

describe("app shell", () => {
  it("opens on home rather than on a conversation", async () => {
    const { router } = await mountApp();

    expect(await screen.findByText("What are we working on?")).toBeInTheDocument();
    expect(router.state.location.pathname).toBe("/");
    // Home used to create a chat on every cold start, leaving an empty one
    // behind whenever the reader did not use it.
    expect(createChat).not.toHaveBeenCalled();
  });

  it("lists recent conversations to pick up again", async () => {
    await mountApp();

    const recent = await screen.findAllByRole("button", { name: "Roadmap" });
    expect(recent.length).toBeGreaterThan(0);
  });

  it("says so when there is nothing to pick up", async () => {
    listChats.mockResolvedValue([]);
    await mountApp();

    expect(await screen.findByText("No chats yet")).toBeInTheDocument();
  });

  it("starts a conversation from the home composer and sends what was written", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByText("What are we working on?");

    await user.type(screen.getByRole("textbox"), "summarise the filing");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => expect(createChat).toHaveBeenCalledOnce());
    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
    // Home writes the message but does not post it — the chat route does, so
    // there is only one send path.
    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith("chat-1", expect.any(String), "summarise the filing"),
    );
  });

  it("opens a conversation from the sidebar", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByText("What are we working on?");

    const recentList = screen.getByLabelText("Chats");
    const [row] = screen
      .getAllByRole("button", { name: "Roadmap" })
      .filter((button) => recentList.contains(button));
    await user.click(row);

    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
    expect(await screen.findByTestId("transcript")).toBeInTheDocument();
  });

  it("arranges panels beside the conversation from the sidebar", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
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
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    await user.click(screen.getByRole("button", { name: "Sources" }));
    await screen.findByTestId("sources");

    await user.click(screen.getByRole("button", { name: "Close" }));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
    expect(screen.queryByTestId("sources")).not.toBeInTheDocument();
  });

  it("restores the arrangement a deep link describes", async () => {
    await mountApp({ at: "/c/chat-2?left=outputs&right=chat" });

    expect(await screen.findByTestId("outputs")).toBeInTheDocument();
  });

  it("finds a conversation through the chats panel's search", async () => {
    const user = userEvent.setup();
    await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");

    await user.click(screen.getByRole("button", { name: "All chats" }));
    const search = await screen.findByRole("textbox", { name: "Search chats" });

    await user.type(search, "roadmap");
    expect(screen.getAllByRole("button", { name: "Roadmap" }).length).toBeGreaterThan(0);

    await user.clear(search);
    await user.type(search, "nothing matches this");
    expect(await screen.findByText("No matches")).toBeInTheDocument();
  });

  it("keeps watching for the agent's questions while settings is open", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    await waitFor(() => expect(listPendingUserQuestions).toHaveBeenCalled());

    await user.click(screen.getByText("Settings"));
    expect(await screen.findByTestId("settings")).toBeInTheDocument();
    await waitFor(() => expect(router.state.location.pathname).toBe("/settings"));
    // The conversation is gone, and that is fine — the transcript rehydrates
    // from a durable journal on the way back.
    expect(screen.queryByTestId("transcript")).not.toBeInTheDocument();

    // What must not stop is being told the agent has parked a turn on a
    // question. The watcher belongs to the shell, so it is still running.
    const readsBefore = listPendingUserQuestions.mock.calls.length;
    listPendingUserQuestions.mockResolvedValue([
      { callId: "call-1", turnId: "turn-1", askedAt: "2026-07-27T00:00:00Z", questions: [] },
    ]);
    usePendingPrompts.getState().refresh();

    await waitFor(() =>
      expect(listPendingUserQuestions.mock.calls.length).toBeGreaterThan(readsBefore),
    );
    await waitFor(() =>
      expect(usePendingPrompts.getState().userQuestions).toHaveLength(1),
    );
    // And the dock still gets told, which is the whole point.
    await waitFor(() => expect(requestUserAttention).toHaveBeenCalled());
  });

  it("returns from settings to the conversation that was open", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    await user.click(screen.getByText("Settings"));
    await screen.findByTestId("settings");

    await user.click(screen.getByRole("button", { name: "Back to app" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
  });
});
