// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { CodeRepoSnapshot, CodeWorkspaceSnapshot } from "@/api/types";
import { panelSearchFrom } from "@/panel/panelUrl";
import { CodeArchivePage } from "./CodeArchivePage";
import { useCodeCatalogStore } from "./CodeCatalogStore";

vi.mock("@/openInBrowser", () => ({ openInBrowser: vi.fn() }));
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));
vi.mock("./CodeSidebar", () => ({ CodeSidebar: () => null }));

const REPO: CodeRepoSnapshot = {
  id: "repo-1",
  root_path: "/tmp/tidebreak",
  display_name: "Tidebreak",
  default_base_ref: "main",
  branch_prefix: "agent",
  quick_actions: [],
  created_at: "2026-08-01T00:00:00.000Z",
};

const ARCHIVED_WORKSPACES: CodeWorkspaceSnapshot[] = [
  {
    id: "ws-transcript",
    repo_id: REPO.id,
    title: "Unify keyboard shortcuts",
    worktree_path: "/tmp/tidebreak/keyboard-shortcuts",
    branch_name: "agent/keyboard-shortcuts",
    base_ref: "main",
    status: "archived",
    created_at: "2026-08-01T00:00:00.000Z",
    archived_at: "2026-08-20T00:00:00.000Z",
  },
  {
    id: "ws-other",
    repo_id: REPO.id,
    title: "Transcript retention notes",
    worktree_path: "/tmp/tidebreak/transcript-retention",
    branch_name: "agent/transcript-retention",
    base_ref: "main",
    status: "released",
    created_at: "2026-07-20T00:00:00.000Z",
    archived_at: "2026-08-18T00:00:00.000Z",
    released_at: "2026-08-19T00:00:00.000Z",
  },
];

afterEach(() => {
  cleanup();
  useCodeCatalogStore.getState().reset();
});

function archiveClient(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    listCodeRepos: vi.fn(async () => [REPO]),
    listCodeWorkspaces: vi.fn(async () => ARCHIVED_WORKSPACES),
    getHarnessDoctor: vi.fn(async () => ({ harnesses: [] })),
    listCodeHarnessModels: vi.fn(async (kind) => ({ kind, models: [] })),
    restoreCodeWorkspace: vi.fn(async (workspaceId) =>
      ARCHIVED_WORKSPACES.find((workspace) => workspace.id === workspaceId),
    ),
    searchCodeWorkspace: vi.fn(async () => ({
      matches: [],
      history_matches: [
        {
          workspace_id: "ws-transcript",
          workspace_title: "Unify keyboard shortcuts",
          session_id: "session-reclaim-notes",
          turn_id: "turn-reclaim-notes",
          source: "turn_user_input",
          preview:
            "Keep the reclaim tiers safe by searching archived conversation history.",
          created_at: "2026-08-20T12:00:00.000Z",
        },
      ],
      truncated: false,
    })),
    ...overrides,
  } as unknown as ApiClient;
}

function appContext(client: ApiClient): AppContextValue {
  return { client } as unknown as AppContextValue;
}

async function renderArchive(client: ApiClient) {
  const rootRoute = createRootRoute();
  const archiveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/archive",
    component: CodeArchivePage,
  });
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    validateSearch: (search: Record<string, unknown>) =>
      panelSearchFrom(search),
    component: () => <p>Workspace destination</p>,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([archiveRoute, workspaceRoute]),
    history: createMemoryHistory({ initialEntries: ["/code/archive"] }),
  });
  await router.load();
  render(
    <AppContextProvider value={appContext(client)}>
      <RouterProvider router={router as never} />
    </AppContextProvider>,
  );
  return router;
}

describe("CodeArchivePage", () => {
  it("finds transcript-only matches and opens the producing session", async () => {
    const client = archiveClient();
    const router = await renderArchive(client);
    const user = userEvent.setup();

    await user.type(
      await screen.findByPlaceholderText(
        "Search workspaces and conversations…",
      ),
      "reclaim tiers",
    );

    await waitFor(() =>
      expect(client.searchCodeWorkspace).toHaveBeenCalledWith("ws-transcript", {
        query: "reclaim tiers",
        history: true,
        limit: 200,
      }),
    );
    expect(client.searchCodeWorkspace).toHaveBeenCalledTimes(1);
    const hit = await screen.findByRole("button", {
      name: /Open conversation in Unify keyboard shortcuts: Keep the reclaim tiers safe/,
    });
    expect(screen.getByText("Unify keyboard shortcuts")).toBeInTheDocument();

    await user.click(hit);

    await waitFor(() => {
      expect(router.state.location.pathname).toBe("/code/w/ws-transcript");
      expect(router.state.location.search).toMatchObject({
        task: "session-reclaim-notes",
      });
    });
  });

  it("keeps metadata matches usable when conversation search fails", async () => {
    const client = archiveClient({
      searchCodeWorkspace: vi.fn(async () => {
        throw new Error("Conversation history is unavailable.");
      }),
    });
    await renderArchive(client);

    await userEvent
      .setup()
      .type(
        await screen.findByPlaceholderText(
          "Search workspaces and conversations…",
        ),
        "retention",
      );

    expect(
      await screen.findByText("Transcript retention notes"),
    ).toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Conversation history is unavailable. Workspace matches are still shown.",
    );
  });
});
