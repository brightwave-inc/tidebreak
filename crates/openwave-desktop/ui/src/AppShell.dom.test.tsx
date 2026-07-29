// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatListStore } from "./ChatListStore";
import { usePendingPrompts } from "./PendingPrompts";
import { useRefreshSignals } from "./RefreshSignals";
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
  {
    id: "chat-1",
    title: "Roadmap",
    model: null,
    reasoning_effort: null,
    citation_format: null,
    project_id: null,
  },
  {
    id: "chat-2",
    title: null,
    model: null,
    reasoning_effort: null,
    citation_format: null,
    project_id: null,
  },
];

const listChats = vi.fn(async () => chats);
const createChat = vi.fn(async () => chats[0]);
const listPendingUserQuestions = vi.fn(async () => [] as unknown[]);
const listPendingFolderAccessRequests = vi.fn(async () => [] as unknown[]);
const listPendingChatPrompts = vi.fn(async () => [] as unknown[]);
const listPendingOutputWritebackRequests = vi.fn(async () => [] as unknown[]);
const requestUserAttention = vi.fn(async () => {});
const postMessage = vi.fn(async () => {});
// Unmanaged is the shape most of these shell tests model; the managed gate
// has its own DOM tests, and the one shell test that flips this to managed
// pins that a gated shell does no work at all.
const unmanaged: import("./api").ManagedPolicy = {
  managed: false,
  source: "unmanaged",
  misconfigured: false,
};
const getPolicy = vi.fn(async () => unmanaged);
const getGatewayStatus = vi.fn(async () => ({
  base_url: "https://gateway.example",
  signed_in: false,
  model_count: 0,
  sign_in: { state: "idle" },
}));

vi.mock("./boot", () => ({
  resolveServerInfo: vi.fn(async () => ({ baseUrl: "http://127.0.0.1:1", token: "t" })),
}));

vi.mock("./api", () => ({
  ApiClient: class {
    getPolicy = getPolicy;
    getGatewayStatus = getGatewayStatus;
    gatewaySignIn = vi.fn(async () => ({ authorization_url: "http://gw/authorize" }));
    listModels = vi.fn(async () => ({ models: [], roles: [] }));
    listProviders = vi.fn(async () => ({ providers: [] }));
    getSettings = vi.fn(async () => ({
      model: null,
      citation_format: "inline",
      has_api_key: false,
    }));
    listChats = listChats;
    createChat = createChat;
    listPendingUserQuestions = listPendingUserQuestions;
    listPendingFolderAccessRequests = listPendingFolderAccessRequests;
    listPendingChatPrompts = listPendingChatPrompts;
    listPendingOutputWritebackRequests = listPendingOutputWritebackRequests;
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

vi.mock("./sources/SourcesView", () => ({
  SourcesView: () => <div data-testid="sources">sources</div>,
}));

vi.mock("./outputs/OutputsView", () => ({
  OutputsView: () => <div data-testid="outputs">outputs</div>,
}));

vi.mock("./FoldersView", () => ({
  FoldersView: () => <div data-testid="folders">folders</div>,
}));

// The settings rail (Back to app, section links) is the real thing here; only
// the section body is stubbed. Providers is where a bare /settings lands, so
// stubbing it is enough to stand in for whichever section the outlet renders.
vi.mock("./settings/ProvidersPanel", () => ({
  ProvidersPanel: () => <div data-testid="settings">settings</div>,
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
  window.localStorage.clear();
  window.location.hash = "";
  listChats.mockClear();
  listChats.mockResolvedValue(chats);
  createChat.mockClear();
  listPendingUserQuestions.mockReset();
  listPendingUserQuestions.mockResolvedValue([]);
  listPendingFolderAccessRequests.mockReset();
  listPendingFolderAccessRequests.mockResolvedValue([]);
  listPendingChatPrompts.mockReset();
  listPendingChatPrompts.mockResolvedValue([]);
  listPendingOutputWritebackRequests.mockReset();
  listPendingOutputWritebackRequests.mockResolvedValue([]);
  requestUserAttention.mockClear();
  postMessage.mockClear();
  getPolicy.mockClear();
  getPolicy.mockResolvedValue(unmanaged);
  getGatewayStatus.mockClear();
  usePendingPrompts.setState({ chatId: null, userQuestions: [], folderAccess: [] });
  // The stores outlive a test file's renders, so a chat list left behind would
  // decide the next test's routing before its own boot ever ran.
  useChatListStore.setState({ chats: [], chatsLoaded: false });
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

  it("marks a parked conversation other than the one that is open", async () => {
    listPendingChatPrompts.mockResolvedValue([
      {
        chatId: "chat-2",
        questionCallIds: ["call-parked"],
        folderAccessCallIds: [],
        outputWritebackCallIds: [],
      },
    ]);
    await mountApp({ at: "/c/chat-1" });

    // The conversation's own rail carries no chat list, so the way back to the
    // others is what reports that one of them is waiting.
    expect(
      await screen.findByLabelText("Another chat needs attention"),
    ).toBeInTheDocument();
    expect(requestUserAttention).toHaveBeenCalledOnce();
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

    // Home also carries the chat explorer's search field, so the composer has
    // to be picked out by name rather than by being the only input.
    await user.type(
      screen.getByRole("textbox", { name: "Message" }),
      "summarise the filing",
    );
    await user.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => expect(createChat).toHaveBeenCalledOnce());
    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
    // Home writes the message but does not post it — the chat route does, so
    // there is only one send path.
    await waitFor(() =>
      expect(postMessage).toHaveBeenCalledWith(
        "chat-1",
        expect.any(String),
        "summarise the filing",
        [],
      ),
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

  it("leaves a conversation to find another one, and searches for it there", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");

    // Finding a chat is something done between conversations, so the way out
    // of one is what leads to it.
    await user.click(screen.getByRole("button", { name: "All chats" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    const search = await screen.findByRole("textbox", { name: "Search chats" });

    await user.type(search, "roadmap");
    expect(screen.getAllByRole("button", { name: "Roadmap" }).length).toBeGreaterThan(0);

    await user.clear(search);
    await user.type(search, "nothing matches this");
    expect(await screen.findByText("No matches")).toBeInTheDocument();
  });

  it("gives each route only the controls that route can act on", async () => {
    const conversationOnly = ["Sources", "Outputs", "Folders"];

    await mountApp();
    await screen.findByText("What are we working on?");
    // Not disabled — absent. Home has no conversation for these to describe,
    // and offering them here is what let the rail navigate into whichever
    // chat happened to have been open last.
    for (const label of conversationOnly) {
      expect(screen.queryByRole("button", { name: label })).not.toBeInTheDocument();
    }
    cleanup();

    await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    for (const label of conversationOnly) {
      expect(screen.getByRole("button", { name: label })).toBeEnabled();
    }
    // And the conversation's rail is not a place to browse the others from.
    expect(screen.queryByLabelText("Chats")).not.toBeInTheDocument();
  });

  it("remembers the collapsed rail across a restart", async () => {
    const user = userEvent.setup();
    await mountApp();
    await screen.findByText("What are we working on?");

    await user.click(screen.getByRole("button", { name: "Collapse sidebar" }));

    expect(useUiStore.getState().sidebarCollapsed).toBe(true);
    expect(window.localStorage.getItem("openwave.sidebar-collapsed")).toBe("true");
  });

  it("keeps watching for the agent's questions while settings is open", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    await waitFor(() => expect(listPendingUserQuestions).toHaveBeenCalled());

    await user.click(screen.getByText("Settings"));
    expect(await screen.findByTestId("settings")).toBeInTheDocument();
    // A bare /settings redirects to the first section, so that is where the URL
    // lands rather than on /settings itself.
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/settings/providers"),
    );
    // The conversation is gone, and that is fine — the transcript rehydrates
    // from a durable journal on the way back.
    expect(screen.queryByTestId("transcript")).not.toBeInTheDocument();

    // What must not stop is being told the agent has parked a turn on a
    // question. The summary watcher belongs to the shell, so it is still
    // running with no conversation open. Prompt detail is not fetched here —
    // settings has no chat, and the one being returned to rehydrates its own.
    const readsBefore = listPendingChatPrompts.mock.calls.length;
    listPendingChatPrompts.mockResolvedValue([
      {
        chatId: "chat-1",
        questionCallIds: ["call-1"],
        folderAccessCallIds: [],
        outputWritebackCallIds: [],
      },
    ]);
    useRefreshSignals.getState().signal("userQuestions");

    await waitFor(() =>
      expect(listPendingChatPrompts.mock.calls.length).toBeGreaterThan(readsBefore),
    );
    // And the dock still gets told, which is the whole point.
    await waitFor(() => expect(requestUserAttention).toHaveBeenCalled());
  });

  it("does no shell work behind the managed sign-in gate", async () => {
    // Managed and signed out: the gate is a hard stop, not a curtain in
    // front of a running app. The prompt watcher must not poll and the
    // shortcuts must not act — Cmd+N creating chats behind the sign-in
    // screen is the regression this pins.
    getPolicy.mockResolvedValue({
      managed: true,
      gateway_url: "https://gateway.example/",
      source: "os",
      misconfigured: false,
    });
    const user = userEvent.setup();
    await mountApp();

    expect(await screen.findByText("Sign in to continue")).toBeInTheDocument();
    await user.keyboard("{Meta>}n{/Meta}");

    expect(createChat).not.toHaveBeenCalled();
    expect(listPendingChatPrompts).not.toHaveBeenCalled();
  });

  it("returns from settings to the conversation that was open", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    await user.click(screen.getByText("Settings"));
    await screen.findByTestId("settings");

    // Browsing sections must not bury the way out: section hops replace the
    // history entry, so one "Back to app" still exits — not a hop back to the
    // previously viewed section.
    await user.click(screen.getByRole("button", { name: "Appearance" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/settings/appearance"),
    );
    await user.click(screen.getByRole("button", { name: "Updates" }));
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/settings/updates"),
    );

    await user.click(screen.getByRole("button", { name: "Back to app" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
  });
});
