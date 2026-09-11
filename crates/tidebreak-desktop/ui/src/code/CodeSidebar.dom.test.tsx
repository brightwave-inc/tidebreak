// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { resetCodeDeliveryHostState } from "./CodeDeliveryStore";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "./CodeUiStore";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeSidebar } from "./CodeSidebar";
import { writeBrowserTabLayout } from "./workspace/browserTabLayout";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), message: vi.fn() },
}));

/**
 * ADR 0030: the code rail must render without initializing chat session
 * stores. This file never imports ChatSessionStore or ChatListStore.
 */

const client = {
  listCodeRepos: vi.fn(async () => [
    {
      id: "repo-1",
      root_path: "/tmp/app",
      display_name: "app",
      default_base_ref: "main",
      branch_prefix: "tidebreak",
      quick_actions: [],
      created_at: "2026-08-15T00:00:00.000Z",
    },
  ]),
  listCodeWorkspaces: vi.fn(async () => [
    {
      id: "ws-1",
      repo_id: "repo-1",
      title: "Fix login",
      worktree_path: "/tmp/app/.worktrees/fix-login",
      branch_name: "tidebreak/fix-login",
      base_ref: "main",
      status: "active" as const,
      created_at: "2026-08-15T00:00:00.000Z",
    },
  ]),
  getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
  getCodeSubscriptionUsage: vi.fn(async () => ({
    source: "model_gateway" as const,
    diagnostics: [],
    providers: [
      {
        id: "anthropic",
        label: "Anthropic Direct",
        accounts: [
          {
            id: "personal",
            label: "Personal",
            is_own: true,
            state: "available",
            updated_at_unix_seconds: Math.floor(Date.now() / 1000),
            windows: [
              {
                key: "7d-fable",
                label: "Weekly (Fable)",
                used_percent: 91,
                resets_at_unix_seconds: Math.floor(Date.now() / 1000) + 3600,
                status: "allowed_warning",
              },
            ],
          },
        ],
      },
    ],
  })),
  listCodeHarnessModels: vi.fn(async () => ({
    kind: "claude_code" as const,
    models: [],
  })),
  getCodeCloneDefaults: vi.fn(async () => ({
    gh_found: false,
    gh_remediation: "gh is not installed.",
  })),
  getGatewayStatus: vi.fn(async () => ({
    signed_in: false,
    model_count: 0,
    sign_in: { state: "idle" as const },
  })),
  getCodeDeliveryRepositories: vi.fn(async () => ({
    capability: {
      found: true,
      authenticated: true,
      viewer_login: "mira-chen",
      remediation: "",
    },
    repositories: [],
    errors: [],
    fetched_at: "2026-08-15T00:00:00.000Z",
  })),
  openCodeUpdates: vi.fn((_onNotice: (notice: unknown) => void) => {
    return {
      close() {},
      addEventListener() {},
      removeEventListener() {},
    } as unknown as WebSocket;
  }),
};

const app: AppContextValue = {
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
  attachment: "local",
  restartForUpdate: async () => {},
};

beforeEach(() => {
  useCodeUiStore.setState({ collapsedWorkspaceGroups: [] });
});

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  resetCodeDeliveryHostState();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    railPrefs: DEFAULT_RAIL_PREFS,
    selectedWorkspaceIds: [],
    selectionAnchorId: null,
    addRepoOpen: false,
    newWorkspaceOpen: false,
    newWorkspaceRepoId: undefined,
  });
  window.localStorage.clear();
});

describe("CodeSidebar", () => {
  it("opens a conversation without a workspace from its live rail row", async () => {
    client.openCodeUpdates.mockImplementationOnce((onNotice) => {
      queueMicrotask(() =>
        onNotice({
          type: "snapshot",
          sessions: [
            {
              workspace: null,
              session: "internal-1",
              can_open_chat: true,
              kind: "interactive",
              harness_kind: "internal",
              lifecycle: "running",
              attention: { state: { type: "working" }, source: "lifecycle" },
              title: "Research feedback",
              turn_count: 1,
            },
          ],
        }),
      );
      return {
        close() {},
        addEventListener() {},
        removeEventListener() {},
      } as unknown as WebSocket;
    });
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Research feedback, Agent working",
      }),
    );
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/s/internal-1"),
    );
  });

  it("lists conversations the server does not mark as chat-openable and opens them on the session route", async () => {
    client.openCodeUpdates.mockImplementationOnce((onNotice) => {
      queueMicrotask(() =>
        onNotice({
          type: "snapshot",
          sessions: [
            {
              workspace: null,
              session: "shared",
              can_open_chat: false,
              kind: "interactive",
              harness_kind: "claude_code",
              lifecycle: "running",
              attention: { state: { type: "working" }, source: "lifecycle" },
              title: "Slack thread on a sandbox engine",
              turn_count: 1,
            },
            {
              workspace: null,
              session: "legacy",
              kind: "interactive",
              lifecycle: "running",
              attention: { state: { type: "working" }, source: "lifecycle" },
              title: "Legacy conversation",
              turn_count: 1,
            },
          ],
        }),
      );
      return {
        close() {},
        addEventListener() {},
        removeEventListener() {},
      } as unknown as WebSocket;
    });
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    expect(
      await screen.findByRole("button", { name: /Legacy conversation/ }),
    ).toBeInTheDocument();
    fireEvent.click(
      await screen.findByRole("button", {
        name: /Slack thread on a sandbox engine/,
      }),
    );
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/s/shared"),
    );
  });

  it("renders the code rail without chat stores initialized", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(
      screen.getByRole("radiogroup", { name: "App mode" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Work" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Code" })).toBeInTheDocument();
    // The repository heading remains below the workspace action toolbar.
    expect(await screen.findByTitle("app")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Workspaces" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Workspace list settings" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Add repo" }),
    ).toBeInTheDocument();
    // The card's name carries what the glyph rail shows, not just the title.
    expect(
      screen.getByRole("button", {
        name: "Fix login · app · tidebreak/fix-login",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "New workspace" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Analytics" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Pull requests" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Archive" })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Delivery alerts" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Notifications" }),
    ).toBeInTheDocument();
    const destinations = within(
      screen.getByRole("navigation", { name: "Code destinations" }),
    ).getAllByRole("button");
    expect(
      destinations.map(
        (button) => button.getAttribute("aria-label") ?? button.textContent,
      ),
    ).toEqual(["Pull requests", "Notifications", "Analytics", "Archive"]);
    expect(
      screen.getByRole("button", { name: "Settings" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Theme: system. Click to change." }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Subscription usage/ }),
    ).not.toBeInTheDocument();
  });

  it.each([
    ["Add repo", "Add a repo"],
    ["New workspace", "New workspace"],
  ])(
    "opens the %s dialog from the workspace toolbar",
    async (buttonName, dialogName) => {
      await renderWithRouter(
        <AppContextProvider value={app}>
          <CodeSidebar />
        </AppContextProvider>,
        { initialUrl: "/code" },
      );
      await screen.findByRole("button", { name: /^Fix login/ });
      const toolbar = screen.getByRole("toolbar", {
        name: "Workspace actions",
      });
      const button = within(toolbar).getByRole("button", { name: buttonName });
      expect(button).toBeVisible();
      fireEvent.click(button);
      const dialog = await screen.findByRole("dialog", { name: dialogName });
      expect(dialog).toBeVisible();
      fireEvent.keyDown(dialog, { key: "Escape" });
      await waitFor(() =>
        expect(screen.queryByRole("dialog", { name: dialogName })).toBeNull(),
      );
    },
  );

  it("re-sorts and persists from the settings popover", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    await screen.findByTitle("app");

    fireEvent.click(
      screen.getByRole("button", { name: "Workspace list settings" }),
    );
    const grouping = await screen.findByRole("radiogroup", {
      name: "Group workspaces",
    });
    expect(
      within(grouping)
        .getAllByRole("radio")
        .map((radio) => radio.textContent),
    ).toEqual(["Repository", "Status"]);
    fireEvent.click(within(grouping).getByRole("radio", { name: "Status" }));

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Idle, 1 workspace" }),
      ).toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("button", { name: "app, 1 workspace" }),
    ).not.toBeInTheDocument();
    expect(
      JSON.parse(
        window.localStorage.getItem("tidebreak.code-rail-prefs") ?? "{}",
      ),
    ).toMatchObject({ sortMode: "by-status" });
  });

  it("opens the workspace context menu from the keyboard and gives focus back", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const card = await screen.findByRole("button", { name: /^Fix login/ });
    card.focus();
    // Shift+F10 and the Menu key reach the card as a plain `contextmenu`
    // event with no pointer position — the same event this fires.
    fireEvent.contextMenu(card);
    const archive = await screen.findByRole("menuitem", { name: "Archive" });
    expect(archive).toBeInTheDocument();
    expect(archive.className).toContain("text-critical");

    fireEvent.keyDown(archive, { key: "Escape" });
    await waitFor(() => expect(card).toHaveFocus());
  });

  it("brands the running digest instead of a stopped remembered sibling", async () => {
    useCodeCatalogStore.getState().rememberSession({
      visibility: "private",
      id: "sess-codex",
      workspace_id: "ws-1",
      kind: "interactive",
      harness_kind: "codex",
      execution_location: "machine",
      permission_mode: "ask",
      fast_mode: false,
      lifecycle: "ended",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-15T00:00:00.000Z",
    });
    client.openCodeUpdates.mockImplementationOnce((onNotice) => {
      queueMicrotask(() =>
        onNotice({
          type: "snapshot",
          sessions: [
            {
              workspace: "ws-1",
              session: "sess-claude",
              kind: "interactive",
              harness_kind: "claude_code",
              lifecycle: "running",
              attention: {
                state: { type: "working" },
                source: "lifecycle",
              },
              title: "Fix login",
              turn_count: 1,
              activity: "agent",
            },
          ],
        }),
      );
      return {
        close() {},
        addEventListener() {},
        removeEventListener() {},
      } as unknown as WebSocket;
    });

    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(await screen.findByTitle("Claude Code")).toBeInTheDocument();
    expect(screen.queryByTitle("Codex")).not.toBeInTheDocument();
    expect(screen.getByText("Agent working")).toBeInTheDocument();
  });

  it("opens a harness subagent without dropping the workspace panels", async () => {
    client.openCodeUpdates.mockImplementationOnce((onNotice) => {
      queueMicrotask(() =>
        onNotice({
          type: "snapshot",
          sessions: [
            {
              workspace: "ws-1",
              session: "sess-1",
              kind: "interactive",
              lifecycle: "running",
              attention: {
                state: { type: "working" },
                source: "lifecycle",
              },
              title: "Fix login",
              turn_count: 2,
              activity: "subagents",
              subagents: [
                {
                  call_id: "toolu-task-1",
                  name: "Audit the parser",
                  status: "running",
                },
              ],
            },
          ],
        }),
      );
      return {
        close() {},
        addEventListener() {},
        removeEventListener() {},
      } as unknown as WebSocket;
    });
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      {
        initialUrl:
          "/code/w/ws-1?tabs=file.README.md,browser.open&active=browser.open",
      },
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Subagent for Fix login: Audit the parser, Running",
      }),
    );

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/code/w/ws-1");
      expect(router.state.location.search).toMatchObject({
        subagent: "toolu-task-1",
        tabs: "file.README.md,browser.open",
        active: "browser.open",
      });
      expect(router.state.location.search).not.toHaveProperty("task");
    });
  });

  it("opens the code home from the Workspaces heading", async () => {
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code/w/ws-1" },
    );

    fireEvent.click(screen.getByRole("button", { name: "Workspaces" }));

    await waitFor(() => expect(router.state.location.pathname).toBe("/code"));
  });

  /**
   * Without this, a workspace whose setup script failed reads exactly like an
   * idle one: the card has no session row and nothing else names the status.
   */
  it("says Setup failed on a card whose setup script did not finish", async () => {
    client.listCodeWorkspaces.mockResolvedValueOnce([
      {
        id: "ws-broken",
        repo_id: "repo-1",
        title: "Broken setup",
        worktree_path: "/tmp/app/.worktrees/broken",
        branch_name: "tidebreak/broken",
        base_ref: "main",
        status: "setup_failed" as const,
        created_at: "2026-08-15T00:00:00.000Z",
      },
    ] as never);
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    expect(await screen.findByText("Setup failed")).toBeInTheDocument();
    // The status reaches a screen reader too, not just the ink.
    expect(
      screen.getByRole("button", {
        name: "Broken setup · Setup failed · app · tidebreak/broken",
      }),
    ).toBeInTheDocument();
  });

  it("cmd-clicks select without navigating, and a pair shows the bulk menu", async () => {
    client.listCodeWorkspaces.mockResolvedValueOnce([
      {
        id: "ws-1",
        repo_id: "repo-1",
        title: "Fix login",
        worktree_path: "/tmp/app/.worktrees/fix-login",
        branch_name: "tidebreak/fix-login",
        base_ref: "main",
        status: "active" as const,
        created_at: "2026-08-15T00:00:00.000Z",
      },
      {
        id: "ws-2",
        repo_id: "repo-1",
        title: "Fix logout",
        worktree_path: "/tmp/app/.worktrees/fix-logout",
        branch_name: "tidebreak/fix-logout",
        base_ref: "main",
        status: "active" as const,
        created_at: "2026-08-16T00:00:00.000Z",
      },
    ]);
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    const first = await screen.findByRole("button", { name: /^Fix login/ });
    const second = await screen.findByRole("button", { name: /^Fix logout/ });
    fireEvent.click(first, { metaKey: true });
    fireEvent.click(second, { metaKey: true });
    expect(router.state.location.pathname).toBe("/code");
    expect(first).toHaveAttribute("aria-selected", "true");
    expect(second).toHaveAttribute("aria-selected", "true");

    fireEvent.contextMenu(first);
    expect(
      await screen.findByRole("menuitem", { name: "Archive 2 workspaces" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Force archive 2 workspaces" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "Rename…" }),
    ).not.toBeInTheDocument();
  });

  it("cmd-clicks include the open workspace", async () => {
    client.listCodeWorkspaces.mockResolvedValueOnce([
      {
        id: "ws-1",
        repo_id: "repo-1",
        title: "Fix login",
        worktree_path: "/tmp/app/.worktrees/fix-login",
        branch_name: "tidebreak/fix-login",
        base_ref: "main",
        status: "active" as const,
        created_at: "2026-08-15T00:00:00.000Z",
      },
      {
        id: "ws-2",
        repo_id: "repo-1",
        title: "Fix logout",
        worktree_path: "/tmp/app/.worktrees/fix-logout",
        branch_name: "tidebreak/fix-logout",
        base_ref: "main",
        status: "active" as const,
        created_at: "2026-08-16T00:00:00.000Z",
      },
    ]);
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code/w/ws-1" },
    );
    const open = await screen.findByRole("button", { name: /^Fix login/ });
    const other = await screen.findByRole("button", { name: /^Fix logout/ });
    fireEvent.click(other, { metaKey: true });
    expect(open).toHaveAttribute("aria-selected", "true");
    expect(other).toHaveAttribute("aria-selected", "true");
  });

  it("clears selection when you click away from the cards", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    const card = await screen.findByRole("button", { name: /^Fix login/ });
    fireEvent.click(card, { metaKey: true });
    expect(card).toHaveAttribute("aria-selected", "true");
    fireEvent.pointerDown(screen.getByRole("button", { name: "Workspaces" }));
    expect(card).not.toHaveAttribute("aria-selected");
  });

  it("restores saved browser tabs when you enter a workspace from the rail", async () => {
    writeBrowserTabLayout("ws-1", {
      tabs: [{ type: "browser", browserId: "saved" }],
      activeIndex: 0,
      fullscreen: false,
    });
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    fireEvent.click(await screen.findByRole("button", { name: /^Fix login/ }));
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "browser.saved",
      }),
    );
  });

  it("keeps open panels when you select the workspace already on screen", async () => {
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      {
        initialUrl:
          "/code/w/ws-1?tabs=browser.open,file.README.md&active=file.README.md",
      },
    );
    fireEvent.click(await screen.findByRole("button", { name: /^Fix login/ }));
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        tabs: "browser.open,file.README.md",
        active: "file.README.md",
      }),
    );
  });

  it("keeps repository and status headings within Local and Slack", async () => {
    seedMixedSources();
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    const local = await screen.findByRole("region", { name: "Local" });
    const slack = screen.getByRole("region", { name: "Slack" });
    expect(local).toHaveAttribute("data-rail-section", "source:local");
    expect(slack).toHaveAttribute("data-rail-section", "source:slack");
    expect(
      within(local)
        .getByRole("button", { name: "app, 1 workspace" })
        .closest("h3"),
    ).toBeInTheDocument();
    expect(
      within(local).getByRole("button", { name: "app, 1 workspace" }),
    ).toBeInTheDocument();
    expect(
      within(local).getByRole("button", { name: "lib, 1 workspace" }),
    ).toBeInTheDocument();
    expect(
      within(slack).getByRole("button", { name: "app, 2 workspaces" }),
    ).toBeInTheDocument();
    expect(
      within(slack).getByRole("button", { name: /^Slack DM thread/ }),
    ).toBeVisible();
    expect(
      within(slack).getByRole("button", { name: /^Slack channel thread/ }),
    ).toBeVisible();
    expect(screen.queryByRole("region", { name: "Slack DM" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Slack channel" })).toBeNull();

    fireEvent.click(
      screen.getByRole("button", { name: "Workspace list settings" }),
    );
    fireEvent.click(await screen.findByRole("radio", { name: "Status" }));
    expect(
      within(local).getByRole("button", { name: "Idle, 2 workspaces" }),
    ).toBeInTheDocument();
    expect(
      within(slack).getByRole("button", { name: "Idle, 2 workspaces" }),
    ).toBeInTheDocument();
    expect(
      within(local).queryByRole("button", { name: "app, 1 workspace" }),
    ).toBeNull();
    expect(
      within(slack).queryByRole("button", { name: "app, 2 workspaces" }),
    ).toBeNull();
  });

  it("preserves a folded child when its source reopens and supports expand all", async () => {
    seedMixedSources();
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    const slack = await screen.findByRole("region", { name: "Slack" });
    const subgroup = within(slack).getByRole("button", {
      name: "app, 2 workspaces",
    });
    const source = within(slack).getByRole("button", {
      name: "Slack, 2 workspaces",
    });
    fireEvent.click(subgroup);
    expect(subgroup).toHaveAttribute("aria-expanded", "false");
    expect(
      screen.queryByRole("button", { name: /^Slack DM thread/ }),
    ).toBeNull();
    expect(screen.getByRole("button", { name: /^Local thread/ })).toBeVisible();
    fireEvent.click(source);
    expect(source).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(source);
    expect(source).toHaveAttribute("aria-expanded", "true");
    expect(subgroup).toHaveAttribute("aria-expanded", "false");
    expect(
      screen.queryByRole("button", { name: /^Slack DM thread/ }),
    ).toBeNull();

    fireEvent.click(
      screen.getByRole("button", { name: "Collapse all groups" }),
    );
    expect(screen.queryByRole("button", { name: /^Local thread/ })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Expand all groups" }));
    expect(screen.getByRole("button", { name: /^Local thread/ })).toBeVisible();
    expect(
      screen.getByRole("button", { name: /^Slack DM thread/ }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /^Slack channel thread/ }),
    ).toBeVisible();
    expect(useCodeUiStore.getState().collapsedWorkspaceGroups).toEqual([]);
  });

  it("skips folded workspaces in select all and shift-click ranges", async () => {
    seedMixedSources();
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    const local = await screen.findByRole("region", { name: "Local" });
    fireEvent.click(
      within(local).getByRole("button", { name: "lib, 1 workspace" }),
    );
    const first = screen.getByRole("button", { name: /^Local thread/ });
    const last = screen.getByRole("button", { name: /^Slack channel thread/ });
    expect(
      screen.queryByRole("button", { name: /^Hidden local thread/ }),
    ).toBeNull();

    fireEvent.keyDown(first, { key: "a", ctrlKey: true });
    expect(useCodeUiStore.getState().selectedWorkspaceIds).toEqual([
      "ws-local",
      "ws-dm",
      "ws-channel",
    ]);
    fireEvent.keyDown(first, { key: "Escape" });
    expect(useCodeUiStore.getState().selectedWorkspaceIds).toEqual([]);
    fireEvent.click(first, { metaKey: true });
    fireEvent.click(last, { shiftKey: true });
    expect(useCodeUiStore.getState().selectedWorkspaceIds).toEqual([
      "ws-local",
      "ws-dm",
      "ws-channel",
    ]);
    expect(router.state.location.pathname).toBe("/code");
  });

  it("archives every selected workspace from the keyboard chord", async () => {
    const archiveCodeWorkspace = vi.fn(
      async (id: string) =>
        ({
          id,
          repo_id: "repo-1",
          title: id,
          worktree_path: `/tmp/app/.worktrees/${id}`,
          branch_name: `tidebreak/${id}`,
          base_ref: "main",
          status: "archived",
          created_at: "2026-08-15T00:00:00.000Z",
        }) as never,
    );
    client.listCodeWorkspaces.mockResolvedValueOnce([
      {
        id: "ws-1",
        repo_id: "repo-1",
        title: "Fix login",
        worktree_path: "/tmp/app/.worktrees/fix-login",
        branch_name: "tidebreak/fix-login",
        base_ref: "main",
        status: "active" as const,
        created_at: "2026-08-15T00:00:00.000Z",
      },
      {
        id: "ws-2",
        repo_id: "repo-1",
        title: "Fix logout",
        worktree_path: "/tmp/app/.worktrees/fix-logout",
        branch_name: "tidebreak/fix-logout",
        base_ref: "main",
        status: "active" as const,
        created_at: "2026-08-16T00:00:00.000Z",
      },
    ]);
    await renderWithRouter(
      <AppContextProvider
        value={{
          ...app,
          client: { ...client, archiveCodeWorkspace } as never,
        }}
      >
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code/w/ws-1" },
    );
    const first = await screen.findByRole("button", { name: /^Fix login/ });
    const second = await screen.findByRole("button", { name: /^Fix logout/ });
    fireEvent.click(second, { metaKey: true });
    expect(first).toHaveAttribute("aria-selected", "true");
    expect(second).toHaveAttribute("aria-selected", "true");

    // Cmd+Shift+A with the rail focused must not widen the selection first.
    fireEvent.keyDown(second, { key: "A", metaKey: true, shiftKey: true });
    expect(useCodeUiStore.getState().selectedWorkspaceIds).toEqual([
      "ws-1",
      "ws-2",
    ]);

    // What the shell does with the chord when the rail holds a selection.
    act(() => useCodeUiStore.getState().requestArchiveSelection());
    const dialog = await screen.findByRole("alertdialog");
    expect(
      within(dialog).getByText("Archive 2 workspaces?"),
    ).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Archive" }));
    await waitFor(() => expect(archiveCodeWorkspace).toHaveBeenCalledTimes(2));
    expect(archiveCodeWorkspace.mock.calls.map(([id]) => id)).toEqual([
      "ws-1",
      "ws-2",
    ]);
    expect(useCodeUiStore.getState().selectedWorkspaceIds).toEqual([]);
  });

  it("opens and clears selection on an unmodified click", async () => {
    const { router } = await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    const card = await screen.findByRole("button", { name: /^Fix login/ });
    fireEvent.click(card, { metaKey: true });
    expect(card).toHaveAttribute("aria-selected", "true");
    fireEvent.click(card);
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/w/ws-1"),
    );
    expect(card).not.toHaveAttribute("aria-selected");
  });
});

function seedMixedSources() {
  const repos = [
    {
      id: "repo-1",
      root_path: "/tmp/app",
      display_name: "app",
      default_base_ref: "main",
      branch_prefix: "tidebreak",
      quick_actions: [],
      created_at: "2026-08-15T00:00:00.000Z",
    },
    {
      id: "repo-2",
      root_path: "/tmp/lib",
      display_name: "lib",
      default_base_ref: "main",
      branch_prefix: "tidebreak",
      quick_actions: [],
      created_at: "2026-08-15T00:00:00.000Z",
    },
  ];
  client.listCodeRepos.mockResolvedValueOnce(repos);
  client.listCodeWorkspaces.mockResolvedValueOnce(
    [
      {
        id: "ws-local",
        repo_id: "repo-1",
        title: "Local thread",
        branch_name: "tidebreak/local",
        created_at: "2026-08-15T00:00:00.000Z",
      },
      {
        id: "ws-hidden",
        repo_id: "repo-2",
        title: "Hidden local thread",
        branch_name: "tidebreak/hidden",
        created_at: "2026-08-15T00:00:00.000Z",
      },
      {
        id: "ws-dm",
        repo_id: "repo-1",
        title: "Slack DM thread",
        branch_name: "tidebreak/dm",
        created_at: "2026-08-16T00:00:00.000Z",
      },
      {
        id: "ws-channel",
        repo_id: "repo-1",
        title: "Slack channel thread",
        branch_name: "tidebreak/channel",
        created_at: "2026-08-17T00:00:00.000Z",
      },
    ].map((workspace) => ({
      ...workspace,
      worktree_path: `/tmp/${workspace.id}`,
      base_ref: "main",
      status: "active" as const,
    })),
  );
  for (const [workspaceId, externalKey] of [
    ["ws-dm", "T0400000:D0898765:dm2"],
    ["ws-channel", "T0400000:C0812345:1724900000.123456"],
  ] as const) {
    useCodeCatalogStore.getState().rememberSession({
      visibility: "private",
      id: `sess-${workspaceId}`,
      workspace_id: workspaceId,
      kind: "interactive",
      harness_kind: "claude_code",
      execution_location: "machine",
      permission_mode: "ask",
      fast_mode: false,
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-15T00:00:00.000Z",
      external_origin: { channel_kind: "slack", external_key: externalKey },
    });
  }
}
