// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { renderWithRouter } from "@/test/router";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { useCodeDeliveryStore } from "./CodeDeliveryStore";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "./CodeUiStore";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeSidebar } from "./CodeSidebar";

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

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  useCodeDeliveryStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    railPrefs: DEFAULT_RAIL_PREFS,
    selectedWorkspaceIds: [],
    selectionAnchorId: null,
  });
  window.localStorage.clear();
});

describe("CodeSidebar", () => {
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
    // One header over one list: the by-repo group header is a label, and the
    // three actions beside "Workspaces" are the whole toolbar.
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
    fireEvent.click(await screen.findByRole("radio", { name: "By created" }));

    // By-created has no group headers, so the repo label goes away and the
    // card grows its repo chip back.
    await waitFor(() =>
      expect(screen.queryByTitle("app")).not.toBeInTheDocument(),
    );
    expect(
      JSON.parse(
        window.localStorage.getItem("tidebreak.code-rail-prefs") ?? "{}",
      ),
    ).toMatchObject({ sortMode: "by-created" });
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

  it("opens a harness subagent through the filtered workspace address", async () => {
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
      { initialUrl: "/code" },
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
