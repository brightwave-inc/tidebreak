// @vitest-environment jsdom
import {
  cleanup,
  render,
  screen,
  within,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { codeRepositories, codeWorkspace } from "@/stories/fixtures";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeArchivePage } from "./CodeArchivePage";

const { client } = vi.hoisted(() => ({
  client: {
    searchCodeWorkspace: vi
      .fn()
      .mockResolvedValue({ history_matches: [], truncated: false }),
    restoreCodeWorkspace: vi.fn().mockResolvedValue({}),
  },
}));
vi.mock("@/AppContext", () => ({ useApp: () => ({ client }) }));
vi.mock("@tanstack/react-router", () => ({ useNavigate: () => vi.fn() }));
vi.mock("@/RouteFrame", () => ({
  RouteFrame: ({ children }: { children: React.ReactNode }) => children,
}));
vi.mock("./CodeSidebar", () => ({ CodeSidebar: () => null }));

beforeEach(() => {
  vi.clearAllMocks();
  useCodeCatalogStore.setState({
    loaded: true,
    error: null,
    repos: [],
    refresh: vi.fn().mockResolvedValue(undefined),
    workspaces: [
      {
        ...codeWorkspace,
        id: "shared",
        title: "Shared archive",
        repo_id: "repo",
        status: "archived",
        read_only: true,
      },
      {
        ...codeWorkspace,
        id: "owned",
        title: "Owned archive",
        repo_id: "repo",
        status: "archived",
        read_only: false,
      },
    ],
  });
});
afterEach(cleanup);

it("keeps shared archives browsable without offering owner restore", () => {
  render(<CodeArchivePage />);
  const shared = screen
    .getByText("Shared archive")
    .closest('[role="listitem"]') as HTMLElement;
  const owned = screen
    .getByText("Owned archive")
    .closest('[role="listitem"]') as HTMLElement;
  expect(
    within(shared).queryByRole("button", { name: "Restore" }),
  ).not.toBeInTheDocument();
  expect(
    within(shared).getByRole("button", { name: "Open Shared archive" }),
  ).toBeVisible();
  expect(within(owned).getByRole("button", { name: "Restore" })).toBeVisible();
});

it("searches owned archive history even when a shared row appears first for the repo", async () => {
  render(<CodeArchivePage />);
  await userEvent
    .setup()
    .type(
      screen.getByPlaceholderText("Search workspaces and conversations…"),
      "archive",
    );
  await waitFor(() =>
    expect(client.searchCodeWorkspace).toHaveBeenCalledWith("owned", {
      query: "archive",
      history: true,
      limit: 200,
    }),
  );
  expect(
    client.searchCodeWorkspace.mock.calls.every(([id]) => id === "owned"),
  ).toBe(true);
});

it.each(["remote:slack-workspace", ""])(
  "offers restore for an archived sandbox workspace at %j",
  (worktreePath) => {
    useCodeCatalogStore.setState({
      workspaces: [
        {
          ...codeWorkspace,
          id: "remote",
          title: "Remote archive",
          status: "archived",
          worktree_path: worktreePath,
          read_only: false,
          is_owner: true,
        },
      ],
    });
    render(<CodeArchivePage />);
    expect(screen.getByRole("button", { name: "Restore" })).toBeVisible();
    expect(
      screen.getByRole("button", { name: "Open Remote archive" }),
    ).toBeVisible();
  },
);

it("restores an owned local archive and removes it from the archive list", async () => {
  const archived = useCodeCatalogStore
    .getState()
    .workspaces.find((workspace) => workspace.id === "owned")!;
  client.restoreCodeWorkspace.mockResolvedValueOnce({
    ...archived,
    status: "active",
    archived_at: null,
  });
  render(<CodeArchivePage />);
  await userEvent
    .setup()
    .click(screen.getByRole("button", { name: "Restore" }));
  expect(client.restoreCodeWorkspace).toHaveBeenCalledExactlyOnceWith("owned");
  await waitFor(() => expect(screen.queryByText("Owned archive")).toBeNull());
  expect(screen.getByText("Shared archive")).toBeVisible();
});

it("names each archive row's repository, never its raw id", () => {
  const repoId = "0f4c2a9e-1b7d-4e55-9a3c-8d21f0c7b6aa";
  const goneId = "7d3e9b10-55aa-4c21-8f0e-2b9d6c4e1a77";
  useCodeCatalogStore.setState({
    repos: [
      {
        ...codeRepositories[0]!,
        id: "mine",
        display_name: "brightwave-inc/mine",
      },
    ],
    workspaces: [
      {
        ...codeWorkspace,
        id: "own",
        title: "Own archive",
        repo_id: "mine",
        status: "archived",
      },
      {
        ...codeWorkspace,
        id: "shared",
        title: "Shared archive",
        repo_id: repoId,
        repo_display_name: "brightwave-inc/tidebreak",
        status: "archived",
        read_only: true,
      },
      {
        ...codeWorkspace,
        id: "gone",
        title: "Orphaned archive",
        repo_id: goneId,
        repo_display_name: undefined,
        status: "archived",
      },
    ],
  });
  render(<CodeArchivePage />);
  const row = (title: string) =>
    screen.getByText(title).closest('[role="listitem"]') as HTMLElement;
  // The column and the narrow-row line both carry the name.
  expect(
    within(row("Own archive")).getAllByText("brightwave-inc/mine"),
  ).toHaveLength(2);
  expect(
    within(row("Shared archive")).getAllByText("brightwave-inc/tidebreak"),
  ).toHaveLength(2);
  expect(within(row("Shared archive")).queryByText(repoId)).toBeNull();
  for (const gone of within(row("Orphaned archive")).getAllByText(
    "Repository 7d3e9b10",
  )) {
    expect(gone.closest("[title]")).toHaveAttribute(
      "title",
      expect.stringContaining(goneId),
    );
  }
});

it("finds a shared archive by its repository name", async () => {
  useCodeCatalogStore.setState({
    workspaces: [
      {
        ...codeWorkspace,
        id: "shared",
        title: "Shared archive",
        repo_id: "0f4c2a9e-1b7d-4e55-9a3c-8d21f0c7b6aa",
        repo_display_name: "brightwave-inc/tidebreak",
        status: "archived",
        read_only: true,
      },
    ],
  });
  render(<CodeArchivePage />);
  await userEvent
    .setup()
    .type(
      screen.getByPlaceholderText("Search workspaces and conversations…"),
      "tidebreak",
    );
  expect(await screen.findByText("Shared archive")).toBeInTheDocument();
});
