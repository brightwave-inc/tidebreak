// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
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
  restartForUpdate: async () => {},
};

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  useCodeDeliveryStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({ railPrefs: DEFAULT_RAIL_PREFS });
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

    expect(screen.getByRole("radiogroup", { name: "App mode" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Work" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Code" })).toBeInTheDocument();
    // The repo list collapsed into the switcher; the by-repo group header is
    // now the way into the repo page.
    expect(
      await screen.findByRole("button", { name: "Open repo app" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Repos" })).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Workspace list settings" }),
    ).toBeInTheDocument();
    // The card's name carries what the glyph rail shows, not just the title.
    expect(
      screen.getByRole("button", { name: "Fix login · app · tidebreak/fix-login" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New workspace" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delivery" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Archive" })).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Notifications" }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("button", {
        name: "Subscription usage, highest window 91% used",
      }),
    ).toBeInTheDocument();
  });

  it("opens the subscription details from the fixed rail footer", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "Subscription usage, highest window 91% used",
      }),
    );
    expect(await screen.findByText("Subscription usage")).toBeInTheDocument();
    expect(screen.getByText("Model Gateway")).toBeInTheDocument();
    expect(screen.getByText("Weekly (Fable)")).toBeInTheDocument();
    expect(screen.getByTitle("91% used")).toHaveTextContent("91%");
    expect(screen.getByRole("progressbar", { name: "Weekly (Fable) usage" })).toHaveAttribute(
      "aria-valuenow",
      "91",
    );
    expect(screen.getByRole("progressbar", { name: "Weekly (Fable) usage" })).toHaveAttribute(
      "aria-valuetext",
      "91% used",
    );
  });

  it("re-sorts and persists from the settings popover", async () => {
    await renderWithRouter(
      <AppContextProvider value={app}>
        <CodeSidebar />
      </AppContextProvider>,
      { initialUrl: "/code" },
    );
    await screen.findByRole("button", { name: "Open repo app" });

    fireEvent.click(
      screen.getByRole("button", { name: "Workspace list settings" }),
    );
    fireEvent.click(
      await screen.findByRole("radio", { name: "By created" }),
    );

    // By-created has no group headers, so the repo header link goes away and
    // the card grows its repo chip back.
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Open repo app" }),
      ).not.toBeInTheDocument(),
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
});
