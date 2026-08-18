// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useChatListStore } from "./ChatListStore";
import { useCodeCatalogStore } from "./code/CodeCatalogStore";
import { useCodeUiStore } from "./code/CodeUiStore";
import { useExperimentalFlags } from "./experimental";
import { usePendingPrompts } from "./PendingPrompts";
import { useProjectListStore } from "./ProjectListStore";
import { useRefreshSignals } from "./RefreshSignals";
import { useChatsSectionState } from "./sidebar/ChatsSection";
import { SIDEBAR_DEFAULT_WIDTH, useUiStore } from "./UiStore";

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
    project_id: null,
    created_at: "2026-07-28T12:00:00Z",
  },
  {
    id: "chat-2",
    title: null,
    model: null,
    reasoning_effort: null,
    project_id: null,
    created_at: "2026-07-27T12:00:00Z",
  },
];

const listChats = vi.fn(async () => chats);
const createChat = vi.fn(async (_model?: unknown, projectId?: string | null) =>
  projectId
    ? {
        id: "chat-new",
        title: null,
        model: null,
        reasoning_effort: null,
        project_id: projectId,
        created_at: "2026-08-13T12:00:00Z",
      }
    : chats[0],
);
const listProjects = vi.fn(async () => [] as unknown[]);
const createProject = vi.fn(async (title: string) => ({
  id: "project-1",
  title,
  attachment_revision: 0,
  root_attachments: [],
  created_at: "2026-08-13T12:00:00Z",
}));
const getSettings = vi.fn(async () => ({
  model: null,
  has_api_key: false,
  code_mode_enabled: false,
}));
const listCodeRepos = vi.fn(async () => [
  {
    id: "repo-1",
    root_path: "/tmp/app",
    display_name: "app",
    default_base_ref: "main",
    branch_prefix: "tidebreak",
    quick_actions: [],
    created_at: "2026-08-16T00:00:00.000Z",
  },
]);
const listCodeWorkspaces = vi.fn(async () => [] as unknown[]);
const getHarnessDoctor = vi.fn(async () => ({ harnesses: [] }));
const listCodeHarnessModels = vi.fn(async () => ({
  kind: "claude_code" as const,
  models: [],
}));
const listPendingUserQuestions = vi.fn(async () => [] as unknown[]);
const listPendingFolderAccessRequests = vi.fn(async () => [] as unknown[]);
const listInbox = vi.fn(async () => [] as unknown[]);
const listPendingOutputWritebackRequests = vi.fn(async () => [] as unknown[]);
const requestUserAttention = vi.fn(async () => {});

/** One waiting question, as the shell's cross-chat read returns it. */
function parked(chatId: string, callId: string) {
  return {
    chatId,
    chatTitle: null,
    turnId: "turn-1",
    callId,
    kind: "question" as const,
    action: null,
    requestedAt: "2026-08-04T00:00:00Z",
  };
}
const postMessage = vi.fn(async () => {});
// Unmanaged is the shape most of these shell tests model; the managed gate
// has its own DOM tests, and the one shell test that flips this to managed
// pins that a gated shell does no work at all.
const unmanaged: import("./api").ManagedPolicy = {
  managed: false,
  source: "unmanaged",
  misconfigured: false,
allow_local_mcp_servers: false,
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
    getSettings = getSettings;
    listCodeRepos = listCodeRepos;
    listCodeWorkspaces = listCodeWorkspaces;
    getHarnessDoctor = getHarnessDoctor;
    listCodeHarnessModels = listCodeHarnessModels;
    openCodeUpdates = vi.fn(() => ({
      close() {},
      addEventListener() {},
      removeEventListener() {},
    }));
    listChats = listChats;
    createChat = createChat;
    listProjects = listProjects;
    createProject = createProject;
    listPendingUserQuestions = listPendingUserQuestions;
    listPendingFolderAccessRequests = listPendingFolderAccessRequests;
    listInbox = listInbox;
    listPendingOutputWritebackRequests = listPendingOutputWritebackRequests;
    postMessage = postMessage;
    openEvents = vi.fn(() => ({ close: vi.fn() }));
  },
}));

vi.mock("./host", () => ({
  hasNativeHost: () => false,
  hasMacOverlayTitlebar: () => false,
  requestUserAttention,
  onPairingChanged: () => () => undefined,
}));

vi.mock("./updates", () => ({
  useDesktopUpdates: () => ({
    state: { status: "idle", version: null },
    check: vi.fn(),
    restart: vi.fn(),
  }),
}));

vi.mock("./ChatView", async () => {
  const { useChatSessionStore } = await import("./ChatSessionStore");
  return {
    ChatView: () => {
      const messages = useChatSessionStore((session) => session.messages);
      return (
        <div data-testid="transcript">
          {messages.flatMap((message) =>
            "text" in message
              ? [<span key={message.id}>{message.text}</span>]
              : [],
          )}
        </div>
      );
    },
  };
});

vi.mock("./outputs/OutputsView", () => ({
  OutputsView: () => <div data-testid="outputs">outputs</div>,
}));

vi.mock("./FoldersView", () => ({
  FoldersView: () => <div data-testid="folders">folders</div>,
}));

vi.mock("./apps/AppsView", () => ({
  AppsView: () => <div data-testid="apps">apps</div>,
}));

// The settings rail (Back to app, section links) is the real thing here; only
// the section body is stubbed. Providers is where a bare /settings lands, so
// stubbing it is enough to stand in for whichever section the outlet renders.
vi.mock("./settings/ProvidersPanel", () => ({
  ProvidersPanel: () => <div data-testid="settings">settings</div>,
}));

vi.mock("./ChatApprovalHydration", async () => {
  const actual = await vi.importActual<typeof import("./ChatApprovalHydration")>(
    "./ChatApprovalHydration",
  );
  return {
    ...actual,
    loadChatApprovalHydration: vi.fn(async () => ({
      transcript: {
        messages: [],
        tool_activity: [],
        terminal_turns: [],
        last_event_seq: 0,
      },
      pendingApprovals: [],
    })),
  };
});

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

let rectSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  window.localStorage.clear();
  window.location.hash = "";
  // jsdom reports zero for every measurement, and the All chats grid is
  // virtualised: a zero-height viewport renders no rows at all. Give it a
  // fixed box so the rows under test actually exist.
  rectSpy = vi
    .spyOn(HTMLElement.prototype, "getBoundingClientRect")
    .mockReturnValue({
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
  listChats.mockClear();
  listChats.mockResolvedValue(chats);
  createChat.mockClear();
  listProjects.mockClear();
  listProjects.mockResolvedValue([]);
  createProject.mockClear();
  listPendingUserQuestions.mockReset();
  listPendingUserQuestions.mockResolvedValue([]);
  listPendingFolderAccessRequests.mockReset();
  listPendingFolderAccessRequests.mockResolvedValue([]);
  listInbox.mockReset();
  listInbox.mockResolvedValue([]);
  listPendingOutputWritebackRequests.mockReset();
  listPendingOutputWritebackRequests.mockResolvedValue([]);
  requestUserAttention.mockClear();
  postMessage.mockClear();
  getPolicy.mockClear();
  getPolicy.mockResolvedValue(unmanaged);
  getGatewayStatus.mockClear();
  getSettings.mockClear();
  getSettings.mockResolvedValue({
    model: null,
    has_api_key: false,
    code_mode_enabled: false,
  });
  listCodeRepos.mockClear();
  listCodeWorkspaces.mockClear();
  listCodeHarnessModels.mockClear();
  // Code mode is opt-in and its catalog outlives a render, so a test that
  // turned it on must not decide the next one's routes.
  useExperimentalFlags.setState({ loaded: false, codeModeEnabled: false });
  useCodeCatalogStore.getState().reset();
  useCodeUiStore.setState({
    newWorkspaceOpen: false,
    newWorkspaceRepoId: undefined,
    addRepoOpen: false,
    reviewSidebarOpen: true,
  });
  usePendingPrompts.setState({ chatId: null, userQuestions: [], folderAccess: [] });
  // The stores outlive a test file's renders, so a chat list left behind would
  // decide the next test's routing before its own boot ever ran.
  useChatListStore.setState({ chats: [], chatsLoaded: false, chatsError: null });
  useProjectListStore.setState({
    projects: [],
    projectsLoaded: false,
    creatingProject: false,
    deletingProjectId: null,
    renamingProjectId: null,
    renameProjectDraft: "",
    savingProjectTitle: false,
    expandedProjectIds: [],
  });
  useUiStore.setState({ sidebarCollapsed: false, sidebarWidth: SIDEBAR_DEFAULT_WIDTH });
  // Module-level chrome state survives a render, so a test that collapsed or
  // filtered the list would otherwise decide the next one's rail.
  useChatsSectionState.setState({ collapsed: false, filtering: false, query: "" });
});
afterEach(() => {
  cleanup();
  rectSpy.mockRestore();
});

describe("app shell", () => {
  // The first mount pays the one-time import of the route tree, which now
  // includes the All chats grid — comfortably over the default timeout on a
  // loaded machine.
  it("opens on home rather than on a conversation", { timeout: 15000 }, async () => {
    const { router } = await mountApp();

    expect(await screen.findByText("Welcome to Tidebreak")).toBeInTheDocument();
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
    const user = userEvent.setup();
    listInbox.mockResolvedValue([parked("chat-2", "call-parked")]);
    await mountApp({ at: "/c/chat-1" });

    // The row itself carries the marker while the list is showing…
    expect(
      await screen.findByLabelText("New chat needs attention"),
    ).toBeInTheDocument();
    expect(requestUserAttention).toHaveBeenCalledOnce();

    // …and collapsing the list moves the report to the section header, so a
    // folded rail cannot hide that something is waiting. (`expanded` singles
    // out the section toggle from the header breadcrumb, which is also
    // named "Chats".)
    await user.click(screen.getByRole("button", { name: "Chats", expanded: true }));
    expect(screen.queryByLabelText("New chat needs attention")).not.toBeInTheDocument();
    expect(
      await screen.findByLabelText("A chat needs attention"),
    ).toBeInTheDocument();
  });

  it("keeps the shell up with a retryable list when loading chats fails", async () => {
    listChats.mockRejectedValue(new Error("database is locked"));
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });

    // A list failure is not a boot failure: the models and providers fetched
    // fine, so the shell stands and the failure is reported where the list
    // would be. The settled load also frees the deep link to redirect home
    // rather than wait on a fetch that already failed.
    expect(await screen.findByText("Welcome to Tidebreak")).toBeInTheDocument();
    await waitFor(() => expect(router.state.location.pathname).toBe("/"));
    expect(
      await screen.findByText(/Could not load chats/),
    ).toBeInTheDocument();

    listChats.mockResolvedValue(chats);
    await user.click(screen.getByRole("button", { name: "Retry" }));

    expect(
      (await screen.findAllByRole("button", { name: "Roadmap" })).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText(/Could not load chats/)).not.toBeInTheDocument();
  });

  it("starts a conversation from the home composer and sends what was written", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByText("Welcome to Tidebreak");

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
        [],
        [],
        false,
      ),
    );
    expect(
      await screen.findByText("summarise the filing"),
    ).toBeInTheDocument();
  });

  it("opens a conversation from the sidebar", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByText("Welcome to Tidebreak");

    const chatList = screen.getByLabelText("Chats");
    const [row] = screen
      .getAllByRole("button", { name: "Roadmap" })
      .filter((button) => chatList.contains(button));
    await user.click(row);

    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-1"));
    expect(await screen.findByTestId("transcript")).toBeInTheDocument();
  });

  it("opens panels as tabs beside the conversation", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");

    // Chat-scoped places are open by default while the conversation owns the
    // canvas, rather than being hidden in the rail.
    await user.click(await screen.findByRole("button", { name: /Folders/ }));

    expect(await screen.findByTestId("folders")).toBeInTheDocument();
    // The conversation stays mounted beside it rather than being replaced.
    expect(screen.getByTestId("transcript")).toBeInTheDocument();
    await waitFor(() => expect(router.state.location.search).toEqual({ tabs: "folders" }));

    // A second place joins the strip and comes forward without closing the
    // first.
    await user.click(screen.getByRole("button", { name: /^Chat activity/ }));
    await user.click(await screen.findByRole("button", { name: /Outputs/ }));

    expect(await screen.findByTestId("outputs")).toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toEqual({
        tabs: "folders,outputs",
        active: "outputs",
      }),
    );
    expect(screen.queryByTestId("folders")).not.toBeInTheDocument();

    // The tab it displaced is still there to go back to.
    await user.click(screen.getByRole("tab", { name: "Folders" }));
    expect(await screen.findByTestId("folders")).toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ tabs: "folders,outputs" }),
    );
  });

  it("closes a panel back to the conversation alone", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1?tabs=outputs" });
    await screen.findByTestId("outputs");

    await user.click(screen.getByRole("button", { name: "Close Outputs" }));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
    expect(screen.queryByTestId("outputs")).not.toBeInTheDocument();
  });

  it("gives the install-wide libraries the whole pane", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");

    // Apps is a page of its own, not a tab beside the conversation.
    await user.click(screen.getByRole("button", { name: "Apps" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/apps"));
    expect(await screen.findByTestId("apps")).toBeInTheDocument();
    expect(screen.queryByTestId("transcript")).not.toBeInTheDocument();
  });

  it("restores the arrangement a deep link describes", async () => {
    await mountApp({ at: "/c/chat-2?tabs=folders,outputs&active=outputs" });

    expect(await screen.findByTestId("outputs")).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Folders" })).toBeInTheDocument();
  });

  // Windows left open across the change, and links shared before it, still
  // carry the retired pair-of-slots grammar.
  it("restores a link written in the retired layout grammar", async () => {
    await mountApp({ at: "/c/chat-2?left=outputs&right=chat" });

    expect(await screen.findByTestId("outputs")).toBeInTheDocument();
  });

  it("filters the rail's chat list in place", async () => {
    const user = userEvent.setup();
    await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");

    // The filter lives behind the list's own options — there is no separate
    // page to go find a chat on.
    await user.click(screen.getByRole("button", { name: "Chat list options" }));
    await user.click(await screen.findByRole("menuitem", { name: "Filter chats" }));
    const search = await screen.findByRole("searchbox", { name: "Filter chats" });

    await user.type(search, "roadmap");
    const chatList = screen.getByLabelText("Chats");
    expect(within(chatList).getByRole("button", { name: "Roadmap" })).toBeInTheDocument();
    expect(
      within(chatList).queryByRole("button", { name: "New chat" }),
    ).not.toBeInTheDocument();

    await user.clear(search);
    await user.type(search, "nothing matches this");
    expect(await screen.findByText("No chat title contains that.")).toBeInTheDocument();
  });

  it("keeps chat-scoped places with the conversation", async () => {
    await mountApp();
    await screen.findByText("Welcome to Tidebreak");
    // Not disabled — absent. Home has no conversation for the chip to
    // describe, and the rail itself carries nothing chat-scoped anymore.
    expect(screen.queryByLabelText("Chat activity")).not.toBeInTheDocument();
    cleanup();

    await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");
    expect(screen.getByLabelText("Chat activity")).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /Outputs/ })).toBeEnabled();
    expect(screen.getByRole("button", { name: /Folders/ })).toBeEnabled();
  });

  it("switches to another conversation from inside one", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp({ at: "/c/chat-1" });
    await screen.findByTestId("transcript");

    // The rail is the chat list, with the open chat marked, so leaving for
    // another conversation is one click, not a trip through home.
    const chatList = await screen.findByLabelText("Chats");
    expect(
      within(chatList).getByRole("button", { name: "Roadmap" }),
    ).toHaveAttribute("aria-current", "page");

    await user.click(within(chatList).getByRole("button", { name: "New chat" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/c/chat-2"));
  });

  it("remembers the collapsed rail across a restart", async () => {
    const user = userEvent.setup();
    await mountApp();
    await screen.findByText("Welcome to Tidebreak");

    await user.click(screen.getByRole("button", { name: "Collapse sidebar" }));

    expect(useUiStore.getState().sidebarCollapsed).toBe(true);
    expect(window.localStorage.getItem("tidebreak.sidebar-collapsed")).toBe("true");
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
    const readsBefore = listInbox.mock.calls.length;
    listInbox.mockResolvedValue([parked("chat-1", "call-1")]);
    useRefreshSignals.getState().signal("userQuestions");

    await waitFor(() =>
      expect(listInbox.mock.calls.length).toBeGreaterThan(readsBefore),
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
    allow_local_mcp_servers: false,
    });
    const user = userEvent.setup();
    await mountApp();

    expect(await screen.findByText("Sign in to continue")).toBeInTheDocument();
    // Ctrl is the command modifier under jsdom's non-mac user agent; pressing
    // Meta here would prove nothing, because nothing is bound to it.
    await user.keyboard("{Control>}n{/Control}");

    expect(createChat).not.toHaveBeenCalled();
    expect(listInbox).not.toHaveBeenCalled();
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

  it("creates a project from the dialog and opens a chat inside it", async () => {
    const user = userEvent.setup();
    const { router } = await mountApp();
    await screen.findByText("Welcome to Tidebreak");

    await user.click(screen.getByRole("button", { name: "New project" }));
    const name = await screen.findByRole("textbox", { name: "Project name" });
    await user.type(name, "Research");
    await user.keyboard("{Meta>}{Enter}{/Meta}");

    await waitFor(() =>
      expect(createProject).toHaveBeenCalledExactlyOnceWith("Research"),
    );
    await waitFor(() =>
      expect(createChat).toHaveBeenCalledWith(undefined, "project-1"),
    );
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/p/project-1/c/chat-new"),
    );
    expect(screen.getByRole("button", { name: "Research" })).toBeInTheDocument();
  });

  it(
    "opens a workspace rather than a chat for Cmd+N in code mode",
    { timeout: 15000 },
    async () => {
      // Shell shortcuts are mode-scoped, and the mode is the route family. The
      // regression this pins is Cmd+N on a /code route creating a chat and
      // navigating the reader out of the mode they were working in.
      getSettings.mockResolvedValue({
        model: null,
        has_api_key: false,
        code_mode_enabled: true,
      });
      const user = userEvent.setup();
      const { router } = await mountApp({ at: "/code" });

      // The registered repo means the rail and the catalog are both up.
      expect(
        await screen.findByRole("button", { name: "app" }),
      ).toBeInTheDocument();

      // jsdom reports a non-mac user agent, so the platform's command modifier
      // here is Ctrl — the same chord the app takes as Cmd on macOS.
      await user.keyboard("{Control>}n{/Control}");

      const dialog = await screen.findByRole("dialog");
      expect(
        within(dialog).getByText(
          "One worktree and one session on the selected repo.",
        ),
      ).toBeInTheDocument();
      expect(createChat).not.toHaveBeenCalled();
      expect(router.state.location.pathname).toBe("/code");
    },
  );
});
