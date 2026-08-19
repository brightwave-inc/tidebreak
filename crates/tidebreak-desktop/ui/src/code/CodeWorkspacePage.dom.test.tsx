// @vitest-environment jsdom
import type { ReactNode } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type {
  CodeSessionSnapshot,
  CodeWorkspaceSnapshot,
  PullRequestDigest,
} from "@/api/types";
import type { PanelSearch } from "@/panel/panelUrl";
import { toast } from "sonner";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { resetCodeSessionRegistry } from "./CodeSessionRegistry";
import { useCodeUiStore } from "./CodeUiStore";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeWorkspacePage } from "./CodeWorkspacePage";

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    write(_data: string, cb?: () => void) {
      cb?.();
    }
    loadAddon() {}
    open() {}
    dispose() {}
    onData() {
      return { dispose() {} };
    }
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

vi.mock("./FileViewer", () => ({
  FileViewer: ({ path }: { path: string }) => (
    <div data-testid="file-viewer">{path}</div>
  ),
}));

vi.mock("@monaco-editor/react", () => ({
  default: () => null,
  loader: { config() {} },
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}));

// The resize library lays out from real element measurements, which jsdom does
// not provide; left alone it registers no regions and renders nothing.
vi.mock("react-resizable-panels", () => ({
  PanelGroup: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  Panel: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  PanelResizeHandle: () => <div />,
}));

const WORKSPACE: CodeWorkspaceSnapshot = {
  id: "ws-1",
  repo_id: "repo-1",
  title: "Fix login",
  worktree_path: "/tmp/app/.worktrees/fix-login",
  branch_name: "tidebreak/fix-login",
  base_ref: "main",
  status: "active" as const,
  created_at: "2026-08-15T00:00:00.000Z",
};

const REPO = {
  id: "repo-1",
  root_path: "/tmp/app",
  display_name: "app",
  default_base_ref: "main",
  branch_prefix: "tidebreak",
  quick_actions: [] as { name: string; command: string; auto_run_on_create: boolean }[],
  created_at: "2026-08-15T00:00:00.000Z",
};

const SESSION: CodeSessionSnapshot = {
  id: "sess-1",
  workspace_id: "ws-1",
  harness_kind: "claude_code" as const,
  permission_mode: "ask" as const,
  lifecycle: "idle" as const,
  attention: {
    state: { type: "done_unreviewed" as const },
    source: "lifecycle" as const,
  },
  unrecognized_event_count: 0,
  created_at: "2026-08-15T00:00:00.000Z",
};

const TURN = {
  id: "turn-1",
  session_id: "sess-1",
  ordinal: 1,
  status: "completed" as const,
  user_input: "list the files",
  attachments: [],
  started_at: "2026-08-15T00:00:00.000Z",
  ended_at: "2026-08-15T00:02:14.000Z",
  diffstat: { files: 2, insertions: 42, deletions: 7, truncated: false },
  usage: {
    input_tokens: 11_000,
    output_tokens: 12,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 0,
  },
};

const PR: PullRequestDigest = {
  number: 41,
  state: "open" as const,
  title: "Fix login flow",
  url: "https://github.com/acme/app/pull/41",
  draft: true,
  head_branch: "tidebreak/fix-login",
  base_branch: "main",
  checks: [
    { name: "ci / rust", bucket: "pass" as const },
    { name: "ci / ui", bucket: "pending" as const },
  ],
};

function makeClient() {
  return {
    getCodeWorkspace: vi.fn(async () => WORKSPACE),
    listCodeWorkspaceSessions: vi.fn(
      async (): Promise<(typeof SESSION)[]> => [],
    ),
    listCodeSessionTurns: vi.fn(async () => [TURN]),
    listCodeApprovals: vi.fn(async () => []),
    openCodeEvents: vi.fn(() => {
      const socket = {
        onopen: null as WebSocket["onopen"],
        close() {},
        addEventListener() {},
        removeEventListener() {},
      } as unknown as WebSocket;
      queueMicrotask(() => socket.onopen?.(new Event("open")));
      return socket;
    }),
    getCodeRepo: vi.fn(async () => REPO),
    archiveCodeWorkspace: vi.fn(async () => ({
      ...WORKSPACE,
      status: "archived" as const,
    })),
    patchCodeWorkspace: vi.fn(async (id: string, body: { title: string }) => ({
      ...WORKSPACE,
      id,
      title: body.title,
    })),
    setCodeAttention: vi.fn(async () => SESSION),
    runCodeWorkspaceAction: vi.fn(async () => ({
      name: "lint",
      success: false,
      exit_code: 1,
      stdout: "oops",
      stderr: "failed",
      timed_out: false,
    })),
    getCodeWorkspacePr: vi.fn(async () => ({
      dirty: false,
      unpushed: false,
      ahead: 0,
      has_upstream: false,
      suggested_commit_message: "",
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    })),
    listCodeRepos: vi.fn(async () => [REPO]),
    listCodeWorkspaces: vi.fn(async () => [WORKSPACE]),
    getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
    listCodeHarnessModels: vi.fn(async () => ({
      kind: "claude_code" as const,
      models: [],
    })),
    getCodeCloneDefaults: vi.fn(async () => ({
      gh_found: false,
      gh_remediation: "gh is not installed.",
    })),
    openCodeUpdates: vi.fn(
      () =>
        ({
          close() {},
          addEventListener() {},
          removeEventListener() {},
        }) as unknown as WebSocket,
    ),
    listCodeWorkspaceTree: vi.fn(async () => ({
      paths: [] as string[],
      truncated: false,
    })),
    listCodeWorkspaceFiles: vi.fn(async () => ({
      files: [],
      truncated: false,
      stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    })),
    getCodeWorkspaceBlob: vi.fn(async () => ({
      path: "src/lib.rs",
      content: "fn main() {}",
      truncated: false,
      binary: false,
    })),
    getCodeWorkspaceDiff: vi.fn(async () => ({
      diff: "",
      truncated: false,
      stat: { files: 0, insertions: 0, deletions: 0, truncated: false },
    })),
    listCodeTerminals: vi.fn(async () => []),
    createCodeTerminal: vi.fn(async () => ({
      id: "term-1",
      workspace_id: "ws-1",
      cols: 80,
      rows: 24,
      ended: false,
      created_at: "2026-08-15T00:00:00.000Z",
    })),
    readCodeTerminal: vi.fn(async () => ({
      id: "term-1",
      workspace_id: "ws-1",
      bytes: "",
      cursor: 0,
      overflow: false,
      truncated: false,
      ended: false,
    })),
    writeCodeTerminal: vi.fn(async () => undefined),
    resizeCodeTerminal: vi.fn(async () => ({
      id: "term-1",
      workspace_id: "ws-1",
      cols: 80,
      rows: 24,
      ended: false,
      created_at: "2026-08-15T00:00:00.000Z",
    })),
  };
}

function appContext(client: ReturnType<typeof makeClient>): AppContextValue {
  return {
    client: client as never,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
    startRename: () => {},
    commitRename: () => {},
    cancelRename: () => {},
    newProject: async () => false,
    deleteProject: () => {},
    startProjectRename: () => {},
    commitProjectRename: () => {},
    cancelProjectRename: () => {},
    newChatInProject: () => {},
    moveChatToProject: () => {},
    updateState: { status: "idle", version: null, error: null, enabled: false },
    updateUpToDate: false,
    checkForUpdate: async () => ({
      status: "idle",
      version: null,
      error: null,
      enabled: false,
    }),
    restartForUpdate: async () => {},
  };
}

/**
 * The workspace route renders the page under test; the repo route renders a
 * marker instead, so an assertion can tell the two apart the way the running
 * app does.
 */
async function mountWorkspace(
  client: ReturnType<typeof makeClient>,
  initialUrl = "/code/w/ws-1",
) {
  const rootRoute = createRootRoute();
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: () => <p>code index</p>,
  });
  const codeRepoRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/r/$repoId",
    component: () => <p>repo page</p>,
  });
  const codeWorkspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    validateSearch: (search: Record<string, unknown>): PanelSearch => ({
      tabs: typeof search.tabs === "string" ? search.tabs : undefined,
      active: typeof search.active === "string" ? search.active : undefined,
      fullscreen:
        typeof search.fullscreen === "string" ? search.fullscreen : undefined,
      split: typeof search.split === "string" ? search.split : undefined,
      splitActive:
        typeof search.splitActive === "string" ? search.splitActive : undefined,
      splitFocused:
        typeof search.splitFocused === "string"
          ? search.splitFocused
          : undefined,
      left: typeof search.left === "string" ? search.left : undefined,
      right: typeof search.right === "string" ? search.right : undefined,
    }),
    component: function WorkspaceRoute() {
      const { workspaceId } = codeWorkspaceRoute.useParams();
      return (
        <AppContextProvider value={appContext(client)}>
          <CodeWorkspacePage workspaceId={workspaceId} />
        </AppContextProvider>
      );
    },
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([
      codeRoute,
      codeRepoRoute,
      codeWorkspaceRoute,
    ]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  await router.load();
  const result = render(<RouterProvider router={router as never} />);
  return { ...result, router };
}

afterEach(() => {
  cleanup();
  resetCodeSessionRegistry();
  useCodeCatalogStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({ reviewSidebarOpen: true, inspectorScope: null });
  useCodeUiStore.setState({ pendingComposerPrompt: null });
});

describe("CodeWorkspacePage", () => {
  it("gives the transcript chat's scrolling frame and closes the turn it hydrated", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const { container } = await mountWorkspace(client);

    expect(
      await screen.findByRole("article", { name: "You" }),
    ).toHaveTextContent("list the files");

    const view = container.querySelector(".message-view");
    expect(view).not.toBeNull();
    // The transcript, not the panel slot, is the scroller: the pane claims its
    // own height, so `.messages` is what overflows.
    expect(view?.querySelector(".messages > .messages-column")).not.toBeNull();

    const pane = view?.closest(".chat-pane");
    expect(pane).not.toBeNull();
    expect(view?.parentElement?.className).toMatch(/flex/);
    expect(view?.nextElementSibling).toContainElement(
      screen.getByRole("button", { name: "Send message" }),
    );

    const seam = await screen.findByRole("group", { name: "Turn finished" });
    expect(seam).toHaveTextContent("2m 14s");
    expect(seam).toHaveTextContent("2 files +42 −7");
    expect(seam).not.toHaveTextContent("in /");
    expect(
      screen.getByRole("button", { name: /Context: 11,012 tokens used/ }),
    ).toBeInTheDocument();
  });

  it("pins the composer to the bottom of a full-height column on an empty start", async () => {
    const client = makeClient();
    const { container } = await mountWorkspace(client);

    expect(
      await screen.findByText("Start a session on this workspace."),
    ).toBeInTheDocument();

    const pane = container.querySelector(".chat-pane");
    expect(pane).not.toBeNull();
    expect(pane).toContainElement(
      screen.getByRole("button", { name: "Send message" }),
    );
    // The start prompt is a flex child of the pane so `mt-auto` on the
    // composer can consume the empty region under the header.
    expect(pane?.firstElementChild?.className).toMatch(/flex-1/);
  });

  it("shows header skeleton bars instead of Workspace and a repo UUID", async () => {
    const client = makeClient();
    client.getCodeWorkspace.mockImplementation(() => new Promise(() => {}));
    await mountWorkspace(client);

    expect(screen.getByTestId("workspace-header-skeleton")).toBeInTheDocument();
    expect(screen.queryByText("Workspace")).not.toBeInTheDocument();
    expect(screen.queryByText("repo-1")).not.toBeInTheDocument();
  });

  it("marks recorded unrecognized engine events with a warning dot", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([
      { ...SESSION, unrecognized_event_count: 3 },
    ]);
    await mountWorkspace(client);

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("unrecognized-event-dot")).toHaveAttribute(
      "aria-label",
      "3 unrecognized engine events recorded in this session",
    );
  });

  it("shows the Codex session model even before its native catalog loads", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([
      {
        ...SESSION,
        harness_kind: "codex",
        model: "gpt-5.6-luna",
      },
    ]);
    client.listCodeHarnessModels.mockResolvedValue({
      kind: "codex",
      models: [],
    } as never);
    await mountWorkspace(client);

    const model = await screen.findByRole("button", {
      name: "Model: GPT 5.6 Luna",
    });
    expect(model).toBeDisabled();
    expect(model).toHaveAttribute(
      "title",
      "Model: GPT 5.6 Luna (set when this session started)",
    );
  });

  it("toggles the review sidebar from the header control", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    await mountWorkspace(client);

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("code-inspector")).toBeInTheDocument();
    expect(useCodeUiStore.getState().reviewSidebarOpen).toBe(true);

    await user.click(screen.getByRole("button", { name: "Review sidebar" }));

    expect(useCodeUiStore.getState().reviewSidebarOpen).toBe(false);
    expect(screen.queryByTestId("code-inspector")).not.toBeInTheDocument();
  });

  it("surfaces a quick-action exit code on the result toast", async () => {
    const client = makeClient();
    client.getCodeRepo.mockResolvedValue({
      ...REPO,
      quick_actions: [
        { name: "lint", command: "pnpm lint", auto_run_on_create: false },
      ],
    });
    const user = userEvent.setup();
    await mountWorkspace(client);

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    expect(await screen.findByRole("menu")).toHaveTextContent("app");
    await user.click(await screen.findByRole("menuitem", { name: "Run: lint" }));

    await waitFor(() =>
      expect(client.runCodeWorkspaceAction).toHaveBeenCalledWith("ws-1", "lint"),
    );
    expect(toast.error).toHaveBeenCalledWith(
      "lint exited 1",
      expect.objectContaining({
        action: expect.objectContaining({ label: "View output" }),
      }),
    );
  });

  it("puts compact PR status and quick commands in the workspace header", async () => {
    const client = makeClient();
    client.getCodeWorkspace.mockResolvedValue({ ...WORKSPACE, pr: PR });
    const user = userEvent.setup();
    await mountWorkspace(client);

    const bar = await screen.findByTestId("pr-action-bar");
    expect(bar).toHaveAttribute("data-variant", "header");
    expect(bar.closest("header")).not.toBeNull();
    expect(bar).toHaveTextContent("#41");
    expect(bar).toHaveTextContent("Draft");
    expect(bar).toHaveTextContent("2 checks");

    await user.click(within(bar).getByRole("button", { name: "Merge" }));
    expect(
      (screen.getByRole("textbox", { name: "Message" }) as HTMLTextAreaElement)
        .value,
    ).toMatch(/Merge pull request #41/);

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    const menu = await screen.findByRole("menu");
    expect(menu).toHaveTextContent("app");
    expect(menu).toHaveTextContent(WORKSPACE.worktree_path);
  });

  it("leaves the archived workspace for its repo", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Workspace actions" }));
    await user.click(await screen.findByRole("menuitem", { name: "Archive" }));
    const confirmation = await screen.findByRole("alertdialog");
    await user.click(
      within(confirmation).getByRole("button", { name: "Archive" }),
    );

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/r/repo-1"),
    );
    expect(client.archiveCodeWorkspace).toHaveBeenCalledWith("ws-1", false);
    expect(
      screen.queryByRole("heading", { name: /Fix login/ }),
    ).not.toBeInTheDocument();
  });

  it("keeps git and comments in the review sidebar, and opens the terminal as a drawer", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    const inspector = screen.getByTestId("code-inspector");
    expect(
      within(inspector).getByRole("tab", { name: "Files" }),
    ).toBeInTheDocument();
    expect(
      within(inspector).getByRole("tab", { name: "Pull request" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Terminal" }));

    expect(await screen.findByTestId("terminal-drawer")).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: /Terminal/i })).not.toBeInTheDocument();
    expect(router.state.location.search).toMatchObject({ tabs: "terminal" });

    await user.click(screen.getByRole("tab", { name: "Pull request" }));
    expect(within(inspector).getByText("No pull request yet")).toBeInTheDocument();
  });

  it("does not promote a stale files catalog into the conversation strip", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=files,terminal",
    );

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("tablist", { name: "Open panels" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: /Terminal/i })).not.toBeInTheDocument();
    expect(screen.getByTestId("terminal-drawer")).toBeInTheDocument();
    expect(router.state.location.search).toMatchObject({
      tabs: "files,terminal",
    });

    const inspector = screen.getByTestId("code-inspector");
    expect(
      within(inspector).getByRole("tab", { name: "Files" }),
    ).toBeInTheDocument();
  });

  it("opens the terminal drawer without creating a files or diff strip tab", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: /Fix login/ }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Terminal" }));

    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ tabs: "terminal" }),
    );
    expect(
      screen.queryByRole("tablist", { name: "Open panels" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Diff" })).not.toBeInTheDocument();
    expect(screen.getByTestId("terminal-drawer")).toBeInTheDocument();
    expect(
      within(screen.getByTestId("code-inspector")).getByRole("tab", {
        name: "Files",
      }),
    ).toBeInTheDocument();
  });

  it("opens a turn diff as a center tab from the review seam", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const user = userEvent.setup();
    await mountWorkspace(client);

    await user.click(
      await screen.findByRole("button", { name: "Review this turn's changes" }),
    );

    expect(
      await screen.findByRole("tab", { name: "Main agent" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Turn diff" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await waitFor(() =>
      expect(client.getCodeWorkspaceDiff).toHaveBeenCalledWith("ws-1", {
        turn: "turn-1",
        file: undefined,
      }),
    );
  });

  it("moves between center tabs with the arrows and names the panel each opens", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    const user = userEvent.setup();
    await mountWorkspace(client);

    await user.click(
      await screen.findByRole("button", { name: "Review this turn's changes" }),
    );

    const diff = await screen.findByRole("tab", { name: "Turn diff" });
    const chat = screen.getByRole("tab", { name: "Main agent" });
    // One tab stop for the strip, and the panel a tab opens says which tab
    // named it — otherwise the content below is orphaned from the control.
    expect(diff).toHaveAttribute("tabindex", "0");
    expect(chat).toHaveAttribute("tabindex", "-1");
    const panel = document.getElementById(
      diff.getAttribute("aria-controls") ?? "",
    );
    expect(panel).not.toBeNull();
    expect(panel).toHaveAttribute("role", "tabpanel");
    expect(panel).toHaveAttribute("aria-labelledby", diff.id);

    diff.focus();
    await user.keyboard("{ArrowLeft}");
    expect(chat).toHaveAttribute("aria-selected", "true");
    expect(chat).toHaveFocus();
    const chatPanel = document.getElementById(
      chat.getAttribute("aria-controls") ?? "",
    );
    expect(chatPanel).toHaveAttribute("aria-labelledby", chat.id);
  });

  it("offers useful right-click actions for center tabs", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,diff.f.src%252Fmain.rs&active=diff.f.src%252Fmain.rs",
    );

    const fileTab = await screen.findByRole("tab", { name: "lib.rs" });
    expect(screen.getByRole("tab", { name: "main.rs (diff)" })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    fireEvent.contextMenu(fileTab);
    const menu = await screen.findByRole("menu");
    expect(within(menu).getByText("lib.rs")).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Copy path" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close tab" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close other tabs" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close tabs to the right" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Close all tabs" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("menuitem", { name: "Copy path" }));
    await waitFor(async () =>
      expect(await window.navigator.clipboard.readText()).toBe("src/lib.rs"),
    );
    expect(toast.success).toHaveBeenCalledWith("Copied path");

    fireEvent.contextMenu(fileTab);
    await user.click(
      await screen.findByRole("menuitem", {
        name: "Close tabs to the right",
      }),
    );
    expect(
      screen.queryByRole("tab", { name: "main.rs (diff)" }),
    ).not.toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "file.src%2Flib.rs",
      }),
    );

    const chatTab = screen.getByRole("tab", { name: "Main agent" });
    fireEvent.contextMenu(chatTab);
    await user.click(
      await screen.findByRole("menuitem", { name: "Close other tabs" }),
    );
    expect(
      screen.getByRole("tablist", { name: "Workspace center" }),
    ).toBeInTheDocument();
    expect(chatTab).toHaveAttribute("aria-selected", "true");
  });

  it("starts on the main-agent tab and opens a file from the visible new-tab control", async () => {
    const client = makeClient();
    client.listCodeWorkspaceTree.mockResolvedValue({
      paths: ["README.md", "src/lib.rs"],
      truncated: false,
    });
    const user = userEvent.setup();
    await mountWorkspace(client);

    const mainAgent = await screen.findByRole("tab", { name: "Main agent" });
    expect(mainAgent).toHaveAttribute("aria-selected", "true");
    const mainPanel = document.getElementById(
      mainAgent.getAttribute("aria-controls") ?? "",
    );
    expect(mainPanel).toHaveAttribute("role", "tabpanel");
    expect(mainPanel).toHaveAttribute("aria-labelledby", mainAgent.id);

    await user.click(screen.getByRole("button", { name: "New tab" }));
    const picker = await screen.findByRole("textbox", {
      name: "Search files by name",
    });
    expect(picker).toHaveFocus();
    await user.click(await screen.findByRole("button", { name: "src/lib.rs" }));

    expect(await screen.findByRole("tab", { name: "lib.rs" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByTestId("file-viewer")).toHaveTextContent("src/lib.rs");
  });

  it("moves and drags file tabs into a reloadable split group", async () => {
    const client = makeClient();
    const user = userEvent.setup();
    const { router } = await mountWorkspace(
      client,
      "/code/w/ws-1?tabs=file.src%252Flib.rs,file.src%252Fmain.rs&active=file.src%252Fmain.rs",
    );

    const mainTab = await screen.findByRole("tab", { name: "main.rs" });
    fireEvent.contextMenu(mainTab);
    await user.click(
      await screen.findByRole("menuitem", { name: "Move to split right" }),
    );

    const splitStrip = await screen.findByRole("tablist", {
      name: "Workspace split",
    });
    expect(
      within(splitStrip).getByRole("tab", { name: "main.rs" }),
    ).toHaveAttribute("aria-selected", "true");
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "file.src%2Flib.rs",
        split: "file.src%2Fmain.rs",
        splitFocused: "1",
      }),
    );

    await user.click(
      within(splitStrip).getByRole("button", {
        name: "Move split tabs to main group",
      }),
    );
    expect(
      screen.queryByRole("tablist", { name: "Workspace split" }),
    ).not.toBeInTheDocument();

    const libTab = screen.getByRole("tab", { name: "lib.rs" });
    const dataTransfer = {
      effectAllowed: "none",
      setData: vi.fn(),
    };
    fireEvent.dragStart(libTab, { dataTransfer });
    const dropZone = await screen.findByTestId("split-drop-zone");
    fireEvent.dragOver(dropZone, { dataTransfer });
    fireEvent.drop(dropZone, { dataTransfer });

    expect(
      await screen.findByRole("tablist", { name: "Workspace split" }),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        split: "file.src%2Flib.rs",
      }),
    );
  });

  it("keeps the jump-to-latest pill out of the tab order until it is on screen", async () => {
    const client = makeClient();
    client.listCodeWorkspaceSessions.mockResolvedValue([SESSION]);
    await mountWorkspace(client);

    await screen.findByRole("button", { name: "Review this turn's changes" });
    // The pill is the keyboard path back to the tail. It is a real button, so
    // it must leave the tab order rather than sit there invisibly focusable.
    const pill = screen.getByLabelText("Scroll to latest");
    expect(pill.tagName).toBe("BUTTON");
    expect(pill).toHaveAttribute("aria-hidden", "true");
    expect(pill).toHaveAttribute("tabindex", "-1");
  });
});
