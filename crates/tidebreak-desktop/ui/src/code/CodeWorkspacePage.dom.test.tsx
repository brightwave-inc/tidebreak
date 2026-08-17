// @vitest-environment jsdom
import type { ReactNode } from "react";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
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
import type { PanelSearch } from "@/panel/panelUrl";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { disconnectCodeUpdates, useCodeUpdatesStore } from "./CodeUpdatesStore";
import { CodeWorkspacePage } from "./CodeWorkspacePage";

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

const WORKSPACE = {
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
  quick_actions: [],
  created_at: "2026-08-15T00:00:00.000Z",
};

function makeClient() {
  return {
    getCodeWorkspace: vi.fn(async () => WORKSPACE),
    listCodeWorkspaceSessions: vi.fn(async () => []),
    getCodeRepo: vi.fn(async () => REPO),
    archiveCodeWorkspace: vi.fn(async () => ({
      ...WORKSPACE,
      status: "archived" as const,
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
async function mountWorkspace(client: ReturnType<typeof makeClient>) {
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
    history: createMemoryHistory({ initialEntries: ["/code/w/ws-1"] }),
  });
  await router.load();
  const result = render(<RouterProvider router={router as never} />);
  return { ...result, router };
}

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
});

describe("CodeWorkspacePage", () => {
  it("leaves the archived workspace for its repo", async () => {
    const client = makeClient();
    const { router } = await mountWorkspace(client);
    const user = userEvent.setup();

    expect(
      await screen.findByRole("heading", { name: "Fix login" }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Archive" }));
    const confirmation = await screen.findByRole("alertdialog");
    await user.click(
      within(confirmation).getByRole("button", { name: "Archive" }),
    );

    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/code/r/repo-1"),
    );
    expect(client.archiveCodeWorkspace).toHaveBeenCalledWith("ws-1", false);
    expect(
      screen.queryByRole("heading", { name: "Fix login" }),
    ).not.toBeInTheDocument();
  });
});
