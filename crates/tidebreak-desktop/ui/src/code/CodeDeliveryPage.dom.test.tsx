// @vitest-environment jsdom
import { StrictMode } from "react";
import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "../api/client";
import {
  deliveryPullRequestDetails,
  deliveryPullRequests,
  stackedDeliveryPullRequests,
  unregisteredDeliveryPullRequests,
  deliveryRepositoriesSnapshot,
  deliveryRunDetails,
  deliveryRuns,
} from "../stories/fixtures";
import {
  CodeDeliveryPage,
  RunDetailSheet,
  codeDeliverySearchFrom,
} from "./CodeDeliveryPage";
import {
  codeDeliveryRepositoryKey,
  deliveryPullRequestPageKey,
  useCodeDeliveryStore,
} from "./CodeDeliveryStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";

afterEach(() => {
  cleanup();
  useCodeDeliveryStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
});

vi.mock("@/openInBrowser", () => ({ openInBrowser: vi.fn() }));
vi.mock("sonner", () => ({
  toast: { success: vi.fn(), warning: vi.fn(), error: vi.fn() },
}));
vi.mock("./CodeSidebar", () => ({ CodeSidebar: () => null }));

function storyClient(): ApiClient {
  return {
    getCodeDeliveryRepositories: async () => deliveryRepositoriesSnapshot,
    queryCodeDeliveryPullRequests: async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      // "All" — every lifecycle at once, which is the view the bug showed up in.
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }),
  } as unknown as ApiClient;
}

function deliveryClient(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    ...storyClient(),
    queryCodeDeliveryRuns: async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryRuns,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }),
    getCodeDeliveryPullRequestDetail: async ({ number }) =>
      deliveryPullRequestDetails[number]!,
    getCodeDeliveryRunDetail: async ({ id }) => deliveryRunDetails[id]!,
    runCodeDeliveryRunAction: async () => ({
      success: true,
      message: "Rerun queued.",
    }),
    ...overrides,
  } as ApiClient;
}

function appContext(client: ApiClient): AppContextValue {
  return { client } as unknown as AppContextValue;
}

function renderList(client = storyClient()) {
  const rootRoute = createRootRoute();
  const route = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/delivery/pull-requests",
    component: () => <CodeDeliveryPage surface="pull_requests" />,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([route]),
    history: createMemoryHistory({
      initialEntries: ["/code/delivery/pull-requests"],
    }),
  });
  render(
    <AppContextProvider value={appContext(client)}>
      <RouterProvider router={router as never} />
    </AppContextProvider>,
  );
}

async function renderDeliveryRoute(
  surface: "pull_requests" | "runs",
  client: ApiClient,
  initialUrl: string,
) {
  const rootRoute = createRootRoute();
  function DeliveryRoute() {
    return (
      <CodeDeliveryPage surface={surface} search={route.useSearch() as never} />
    );
  }
  const route = createRoute({
    getParentRoute: () => rootRoute,
    path:
      surface === "pull_requests"
        ? "/code/delivery/pull-requests"
        : "/code/delivery/runs",
    validateSearch: codeDeliverySearchFrom,
    component: DeliveryRoute,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([route]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
  await router.load();
  render(
    <AppContextProvider value={appContext(client)}>
      <RouterProvider router={router as never} />
    </AppContextProvider>,
  );
  return router;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function rowFor(title: string): Promise<HTMLElement> {
  const labels = await screen.findAllByText(title, {}, { timeout: 3000 });
  for (const label of labels) {
    const row = label.closest('[role="listitem"]');
    if (row instanceof HTMLElement) return row;
  }
  throw new Error(`no row for ${title}`);
}

describe("delivery pull request list", () => {
  it("fills the available width before a pull request is open", async () => {
    renderList();

    const list = await screen.findByRole("list", { name: "Pull requests" });
    expect(list.parentElement).toHaveClass("min-w-0", "flex-1");
  });

  it("fills the available width while pull requests load", async () => {
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests: async () => deferred().promise,
    } as unknown as ApiClient);

    const skeleton = await screen.findByRole("status", { name: "Loading" });
    expect(skeleton).toHaveClass("w-full", "min-w-0", "flex-1");
  });

  it("re-reads when the server nudges the delivery channel", async () => {
    // The server says when the pull-request store moved (decision 66). The
    // list is a projection of that nudge, not a clock of its own: without
    // the subscription a fix turn's result waits for the reader to press
    // Refresh (issue 2799).
    const queryCodeDeliveryPullRequests = vi.fn(async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }));
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests,
    } as unknown as ApiClient);

    await waitFor(() =>
      expect(queryCodeDeliveryPullRequests).toHaveBeenCalledTimes(1),
    );
    useCodeUpdatesStore.getState().apply({ type: "delivery" });
    await waitFor(() =>
      expect(queryCodeDeliveryPullRequests).toHaveBeenCalledTimes(2),
    );
  });

  it("opens repository-scoped trigger rules from the production dialog", async () => {
    const user = userEvent.setup();
    const listCodeTriggers = vi.fn(async () => []);
    const client = {
      ...storyClient(),
      listCodeTriggers,
      createCodeTrigger: vi.fn(),
      setCodeTriggerEnabled: vi.fn(),
      deleteCodeTrigger: vi.fn(),
    } as unknown as ApiClient;
    renderList(client);

    await user.click(
      await screen.findByRole("button", { name: "Repositories" }),
    );
    await user.click(
      (await screen.findAllByRole("button", { name: "Triggers" }))[0]!,
    );

    expect(
      await screen.findByRole("heading", { name: "Triggers" }),
    ).toBeInTheDocument();
    expect(listCodeTriggers).toHaveBeenCalledWith(
      deliveryRepositoriesSnapshot.repositories[0]!.tidebreak_repo_id,
    );
  });

  it("does not warn when only local-only repositories were skipped", async () => {
    renderList({
      ...storyClient(),
      getCodeDeliveryRepositories: async () => ({
        ...deliveryRepositoriesSnapshot,
        errors: [
          {
            kind: "not_github",
            message:
              "code-mode-audit-IWF86H: could not read origin remote: error: No such remote 'origin'",
          },
          {
            kind: "not_github",
            message:
              "code-mode-audit-fixture: use owner/repo, host/owner/repo, or a GitHub URL",
          },
        ],
      }),
    } as unknown as ApiClient);

    expect(
      await screen.findByText("Build the delivery center"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/could not be refreshed/)).toBeNull();
    expect(screen.queryByText(/code-mode-audit/)).toBeNull();
  });

  it("still names GitHub repositories that failed to refresh", async () => {
    renderList({
      ...storyClient(),
      getCodeDeliveryRepositories: async () => ({
        ...deliveryRepositoriesSnapshot,
        errors: [
          {
            kind: "not_github",
            message: "code-mode-audit-fixture: not a GitHub remote",
          },
          {
            repository: {
              host: "github.com",
              owner: "brightwave-inc",
              name: "docs",
            },
            kind: "transient",
            message: "brightwave-inc/docs did not answer in time.",
          },
        ],
      }),
    } as unknown as ApiClient);

    expect(
      await screen.findByText("brightwave-inc/docs did not answer in time."),
    ).toBeInTheDocument();
    expect(screen.queryByText(/code-mode-audit/)).toBeNull();
  });

  it("opens on your own open pull requests, drafts included", async () => {
    const query = vi.fn(async (_body: unknown) => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }));
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests: query,
    } as unknown as ApiClient);

    expect(await screen.findByRole("button", { name: "Yours" })).toBeTruthy();
    // `states: ["open"]` and nothing else: a draft is open on the wire, and
    // neither the attention nor the ready gate may narrow the first screen.
    await waitFor(() =>
      expect(query.mock.calls.at(-1)?.[0]).toMatchObject({
        authors: ["mara"],
        states: ["open"],
        attention_only: false,
        ready_only: false,
      }),
    );
  });

  it("drops the viewer view when gh cannot report a login", async () => {
    const query = vi.fn(async (_body: unknown) => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }));
    renderList({
      ...storyClient(),
      getCodeDeliveryRepositories: async () => ({
        ...deliveryRepositoriesSnapshot,
        capability: {
          ...deliveryRepositoriesSnapshot.capability,
          viewer_login: undefined,
        },
      }),
      queryCodeDeliveryPullRequests: query,
    } as unknown as ApiClient);

    // "Yours" without a login would read as everybody's, so it goes and the
    // attention view takes the default back.
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Yours" })).toBeNull(),
    );
    await waitFor(() =>
      expect(query.mock.calls.at(-1)?.[0]).toMatchObject({
        authors: [],
        attention_only: true,
      }),
    );
  });

  it("paints the last successful page before the live query returns", async () => {
    const held = deferred<{
      capability: typeof deliveryRepositoriesSnapshot.capability;
      items: typeof deliveryPullRequests;
      errors: never[];
      fetched_at: string;
    }>();
    const repoKeys = deliveryRepositoriesSnapshot.repositories.map(
      codeDeliveryRepositoryKey,
    );
    const baseFilters = {
      search: "",
      repositoryKeys: [] as string[],
      states: ["open"],
      reviewStates: [] as string[],
      checkStates: [] as string[],
      authors: [] as string[],
      attentionOnly: false,
      readyOnly: false,
    };
    useCodeDeliveryStore.setState({
      repositorySnapshot: deliveryRepositoriesSnapshot,
      repositoryFetchedAt: Date.now(),
    });
    for (const authors of [[], ["mara"]]) {
      useCodeDeliveryStore.getState().rememberPullRequestPage({
        key: deliveryPullRequestPageKey(repoKeys, {
          ...baseFilters,
          authors,
        }),
        items: [deliveryPullRequests[0]!],
        fetchedAt: "2026-08-20T14:00:00.000Z",
        errors: [],
      });
    }
    const query = vi.fn(async () => held.promise);
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests: query,
    } as unknown as ApiClient);

    expect(
      await screen.findByText("Build the delivery center"),
    ).toBeInTheDocument();
    expect(screen.queryByText("Make workspace deep links durable")).toBeNull();
    held.resolve({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    });
    expect(
      await screen.findByText("Make workspace deep links durable"),
    ).toBeInTheDocument();
  });

  it("opens a selected pull request in a side pane", async () => {
    const user = userEvent.setup();
    await renderDeliveryRoute(
      "pull_requests",
      deliveryClient(),
      "/code/delivery/pull-requests?view=all",
    );
    await user.click(await rowFor("Build the delivery center"));
    const pane = await screen.findByTestId("pull-request-detail-pane");
    expect(pane).toHaveTextContent("Build the delivery center");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("moves a pull request when its opened detail reports a new state", async () => {
    const user = userEvent.setup();
    const listed = deliveryPullRequests[0]!;
    const merged = {
      ...listed,
      state: "merged" as const,
      merged_at: "2026-08-27T19:45:00.000Z",
      updated_at: "2026-08-27T19:45:00.000Z",
    };
    const client = deliveryClient({
      queryCodeDeliveryPullRequests: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: [listed],
        errors: [],
        fetched_at: "2026-08-27T19:44:00.000Z",
      }),
      getCodeDeliveryPullRequestDetail: async () => ({
        ...deliveryPullRequestDetails[2251]!,
        summary: merged,
      }),
    });
    await renderDeliveryRoute(
      "pull_requests",
      client,
      "/code/delivery/pull-requests?view=all",
    );

    expect(await rowFor(listed.title)).toHaveAttribute(
      "data-status-group",
      "attention",
    );
    await user.click(await rowFor(listed.title));
    const detail = await screen.findByTestId("pull-request-detail-pane");
    expect(
      within(detail).getByLabelText("Pull request summary"),
    ).toHaveTextContent("Merged");
    await waitFor(async () => {
      expect(await rowFor(listed.title)).toHaveAttribute(
        "data-status-group",
        "done",
      );
    });
    expect(
      useCodeDeliveryStore
        .getState()
        .lastPullRequestPages.flatMap((page) => page.items)
        .find((item) => item.id === listed.id),
    ).toMatchObject({ state: "merged" });
    expect(
      screen.getByText("Done").closest('[data-pull-request-group="Done"]'),
    ).not.toBeNull();
  });

  it("moves the open pane with ArrowDown and ArrowUp", async () => {
    const user = userEvent.setup();
    await renderDeliveryRoute(
      "pull_requests",
      deliveryClient(),
      "/code/delivery/pull-requests?view=all",
    );
    await user.click(await rowFor("Build the delivery center"));
    await screen.findByTestId("pull-request-detail-pane");
    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(screen.getByTestId("pull-request-detail-pane")).toHaveTextContent(
        "Adopt the shared status tone map",
      ),
    );
    await user.keyboard("{ArrowUp}");
    await waitFor(() =>
      expect(screen.getByTestId("pull-request-detail-pane")).toHaveTextContent(
        "Build the delivery center",
      ),
    );
  });

  it("reuses cached detail when arrow navigation returns to a pull request", async () => {
    const user = userEvent.setup();
    const getDetail = vi.fn(async ({ number }: { number: number }) =>
      Promise.resolve(deliveryPullRequestDetails[number]!),
    );
    await renderDeliveryRoute(
      "pull_requests",
      deliveryClient({ getCodeDeliveryPullRequestDetail: getDetail as never }),
      "/code/delivery/pull-requests?view=all",
    );

    await user.click(await rowFor("Build the delivery center"));
    await screen.findByRole("tab", { name: /Conversation/ });
    await user.keyboard("{ArrowDown}");
    await waitFor(() =>
      expect(
        getDetail.mock.calls.some(([request]) => request.number === 2229),
      ).toBe(true),
    );
    await screen.findByRole("tab", { name: /Conversation/ });
    await user.keyboard("{ArrowUp}");
    await new Promise((resolve) => window.setTimeout(resolve, 180));

    expect(
      getDetail.mock.calls.filter(([request]) => request.number === 2251),
    ).toHaveLength(1);
    expect(screen.getByTestId("pull-request-detail-pane")).toHaveTextContent(
      "Build the delivery center",
    );
  });

  it("debounces uncached detail reads during rapid arrow navigation", async () => {
    const user = userEvent.setup();
    const getDetail = vi.fn(async ({ number }: { number: number }) =>
      Promise.resolve(deliveryPullRequestDetails[number]!),
    );
    await renderDeliveryRoute(
      "pull_requests",
      deliveryClient({ getCodeDeliveryPullRequestDetail: getDetail as never }),
      "/code/delivery/pull-requests?view=all",
    );

    await user.click(await rowFor("Build the delivery center"));
    await screen.findByRole("tab", { name: /Conversation/ });
    await user.keyboard("{ArrowDown}{ArrowUp}");
    await new Promise((resolve) => window.setTimeout(resolve, 180));

    expect(
      getDetail.mock.calls.filter(([request]) => request.number === 2229),
    ).toHaveLength(0);
    expect(
      getDetail.mock.calls.filter(([request]) => request.number === 2251),
    ).toHaveLength(1);
  });

  it("leaves the open pane alone while typing a comment", async () => {
    const user = userEvent.setup();
    await renderDeliveryRoute(
      "pull_requests",
      deliveryClient(),
      "/code/delivery/pull-requests?view=all",
    );
    await user.click(await rowFor("Build the delivery center"));
    const comment = await screen.findByRole("textbox", {
      name: "Comment on this pull request",
    });
    await user.click(comment);
    await user.keyboard("{ArrowDown}");
    expect(screen.getByTestId("pull-request-detail-pane")).toHaveTextContent(
      "Build the delivery center",
    );
  });

  it("names the author on every row, with a face beside it", async () => {
    renderList();
    const mara = await rowFor("Build the delivery center");
    expect(within(mara).getByText("mara")).toBeInTheDocument();
    // The avatar sits next to the login it belongs to, so it is decorative
    // and carries no alt text — find it by source. These summaries have no
    // avatar URL on the wire, which is the case that has to derive one.
    expect(
      mara.querySelector('img[src^="https://github.com/mara.png"]'),
    ).not.toBeNull();

    const devon = await rowFor("Make workspace deep links durable");
    expect(within(devon).getByText("devon")).toBeInTheDocument();
    expect(
      devon.querySelector('img[src^="https://github.com/devon.png"]'),
    ).not.toBeNull();
  });

  it("filters by a picked author instead of a memorized login", async () => {
    const user = userEvent.setup();
    const query = vi.fn(async (_body: unknown) => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryPullRequests,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }));
    const client = {
      ...storyClient(),
      queryCodeDeliveryPullRequests: query,
    } as unknown as ApiClient;
    useCodeDeliveryStore.setState({
      knownAuthors: [
        { login: "mara", avatarUrl: "https://avatars.test/mara" },
        { login: "devon" },
      ],
    });
    renderList(client);

    // Off the viewer view first: it already carries the signed-in login, and
    // this is about picking somebody.
    await user.click(await screen.findByRole("button", { name: "Open" }));
    await user.click(await screen.findByRole("button", { name: /Filters/ }));
    await user.click(await screen.findByRole("checkbox", { name: "mara" }));
    await waitFor(() =>
      expect(query.mock.calls.at(-1)?.[0]).toMatchObject({ authors: ["mara"] }),
    );

    // A login the pool has never seen still works, typed and entered.
    await user.type(
      screen.getByRole("textbox", { name: "Search authors" }),
      "octocat{Enter}",
    );
    await waitFor(() =>
      expect(query.mock.calls.at(-1)?.[0]).toMatchObject({
        authors: ["mara", "octocat"],
      }),
    );
    expect(screen.getByRole("checkbox", { name: "octocat" })).toBeChecked();
  });

  // The reported bug: every settled pull request rendered "Review Pending",
  // because GitHub clears the review decision once one merges or closes.
  it("labels merged and closed pull requests by their outcome", async () => {
    renderList();
    expect(
      within(
        await rowFor("Cache the workspace digest between polls"),
      ).getByText("Merged"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Rewrite the deployment runbook")).getByText(
        "Closed",
      ),
    ).toBeInTheDocument();
    // Merged, but the host reported CLOSED. Only the merge timestamp says so.
    expect(
      within(await rowFor("Split the workspace route")).getByText("Merged"),
    ).toBeInTheDocument();
    expect(screen.queryByText("Review Pending")).toBeNull();
  });

  it("reports the next useful state on a live pull request", async () => {
    renderList();
    expect(
      within(await rowFor("Build the delivery center")).getByText(
        "Changes requested",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByText(
        "Ready to merge",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Document managed deployments")).getByText("Draft"),
    ).toBeInTheDocument();
  });

  it("names the lifecycle for a screen reader on every row", async () => {
    renderList();
    expect(
      within(
        await rowFor("Cache the workspace digest between polls"),
      ).getByText("Merged:"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Rewrite the deployment runbook")).getByText(
        "Closed:",
      ),
    ).toBeInTheDocument();
  });

  it("uses the pull request mark itself for attention and handoff state", async () => {
    renderList();
    const ready = await rowFor("Make workspace deep links durable");
    expect(ready).toHaveAttribute("data-status-group", "ready");
    expect(within(ready).getByText("Ready to merge:")).toBeInTheDocument();

    const attention = await rowFor("Build the delivery center");
    expect(attention).toHaveAttribute("data-status-group", "attention");
    expect(
      within(attention).getByText("Changes requested:"),
    ).toBeInTheDocument();
    expect(within(attention).queryByLabelText("Needs attention")).toBeNull();
  });

  it("groups attention first and can regroup by repository", async () => {
    const user = userEvent.setup();
    renderList();

    expect(await screen.findByText("Needs your attention")).toBeInTheDocument();
    await user.click(
      await screen.findByRole("combobox", { name: "Group pull requests" }),
    );
    await user.click(
      await screen.findByRole("option", { name: "Group: repository" }),
    );
    expect(
      document.querySelector(
        '[data-pull-request-group="brightwave-inc/tidebreak"]',
      ),
    ).not.toBeNull();
    expect(
      document.querySelector('[data-pull-request-group="brightwave-inc/docs"]'),
    ).not.toBeNull();
  });

  it("summarizes the checks per row", async () => {
    renderList();
    expect(
      within(await rowFor("Build the delivery center")).getByText("1 failed"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByText(
        "2 passed",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Document managed deployments")).getByText(
        "1 pending",
      ),
    ).toBeInTheDocument();
  });

  it("offers one host merge action per live row", async () => {
    renderList();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByRole(
        "button",
        { name: "Merge" },
      ),
    ).toBeEnabled();
    expect(
      within(await rowFor("Build the delivery center")).getByRole("button", {
        name: "Enable auto-merge",
      }),
    ).toBeEnabled();
    expect(
      within(
        await rowFor("Let the catalog say which scopes a server accepts"),
      ).getByRole("button", { name: "Merge when ready" }),
    ).toBeEnabled();
    expect(
      within(
        await rowFor("Ask a hosted MCP server which scopes it accepts"),
      ).queryByRole("button", {
        name: /Merge|Enable auto-merge|Merge when ready/,
      }),
    ).toBeNull();
    expect(
      within(
        await rowFor("Cache the workspace digest between polls"),
      ).queryByRole("button", {
        name: /Merge|Enable auto-merge|Merge when ready/,
      }),
    ).toBeNull();
    expect(
      within(
        await rowFor("Apply reasoning effort changes to the next turn"),
      ).queryByRole("button", {
        name: /Merge|Enable auto-merge|Merge when ready/,
      }),
    ).toBeNull();
  });

  it("drops Enable auto-merge when the open pane reports auto-merge is on", async () => {
    const user = userEvent.setup();
    const listItem = {
      ...deliveryPullRequests[0]!,
      auto_merge_enabled: false,
      checks: [{ name: "ci", bucket: "pending" as const }],
      attention_reasons: [],
    };
    const client = deliveryClient({
      queryCodeDeliveryPullRequests: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: [listItem],
        errors: [],
        fetched_at: "2026-08-20T15:20:00.000Z",
      }),
      getCodeDeliveryPullRequestDetail: async () => ({
        ...deliveryPullRequestDetails[2251]!,
        summary: {
          ...listItem,
          auto_merge_enabled: true,
        },
      }),
    });
    await renderDeliveryRoute(
      "pull_requests",
      client,
      "/code/delivery/pull-requests?view=all",
    );
    const row = await rowFor("Build the delivery center");
    expect(
      within(row).getByRole("button", { name: "Enable auto-merge" }),
    ).toBeInTheDocument();
    await user.click(row);
    await screen.findByTestId("pull-request-detail-pane");
    await waitFor(async () => {
      const next = await rowFor("Build the delivery center");
      expect(
        within(next).queryByRole("button", { name: "Enable auto-merge" }),
      ).toBeNull();
    });
  });

  it("merges a ready row through the delivery API after confirm", async () => {
    const user = userEvent.setup();
    const runAction = vi.fn(async (_body: unknown) => ({
      success: true,
      message: "Pull request #2247 merged",
    }));
    renderList(
      deliveryClient({
        runCodeDeliveryPullRequestAction: runAction,
      }),
    );

    await user.click(
      within(await rowFor("Make workspace deep links durable")).getByRole(
        "button",
        { name: "Merge" },
      ),
    );
    expect(runAction).not.toHaveBeenCalled();
    await user.click(
      within(screen.getByRole("alertdialog")).getByRole("button", {
        name: "Merge",
      }),
    );
    await waitFor(() => expect(runAction).toHaveBeenCalledTimes(1));
    expect(runAction.mock.calls[0]![0]).toMatchObject({
      target: { number: 2247 },
      action: {
        type: "merge",
        auto: false,
        admin: false,
        expected_head_sha: "73fc201",
      },
    });
    expect(toast.success).toHaveBeenCalled();
  });

  it("shows comment counts without opening the pull request", async () => {
    renderList();
    expect(
      within(await rowFor("Build the delivery center")).getByText("3 comments"),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByText(
        "None",
      ),
    ).toBeInTheDocument();
  });

  it("loads a changed pull-request target within the same repository", async () => {
    const getDetail = vi.fn(async ({ number }: { number: number }) =>
      Promise.resolve(deliveryPullRequestDetails[number]!),
    );
    const client = deliveryClient({
      queryCodeDeliveryPullRequests: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: [],
        errors: [],
        fetched_at: "2026-08-20T15:20:00.000Z",
      }),
      getCodeDeliveryPullRequestDetail: getDetail as never,
    });
    const router = await renderDeliveryRoute(
      "pull_requests",
      client,
      "/code/delivery/pull-requests?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&pr=2251",
    );
    expect(
      await screen.findByRole("heading", { name: "Build the delivery center" }),
    ).toBeInTheDocument();

    await router.navigate({
      to: "/code/delivery/pull-requests",
      search: {
        repoHost: "github.com",
        repoOwner: "brightwave-inc",
        repoName: "tidebreak",
        pr: 2247,
      },
    });

    expect(
      await screen.findByRole("heading", {
        name: "Make workspace deep links durable",
      }),
    ).toBeInTheDocument();
    expect(getDetail.mock.calls.map(([target]) => target.number)).toEqual(
      expect.arrayContaining([2251, 2247]),
    );
  });

  it("reuses a pending pull-request detail request when the target row is selected", async () => {
    const user = userEvent.setup();
    const detail =
      deferred<
        Awaited<ReturnType<ApiClient["getCodeDeliveryPullRequestDetail"]>>
      >();
    const getDetail = vi.fn(() => detail.promise);
    const client = deliveryClient({
      getCodeDeliveryPullRequestDetail: getDetail,
    });
    await renderDeliveryRoute(
      "pull_requests",
      client,
      "/code/delivery/pull-requests?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&pr=2251",
    );

    await user.click(await rowFor("Build the delivery center"));
    await new Promise((resolve) => window.setTimeout(resolve, 20));
    expect(getDetail).toHaveBeenCalledTimes(1);

    detail.resolve(deliveryPullRequestDetails[2251]!);
    expect(
      await screen.findByRole("tab", { name: /Conversation/ }),
    ).toBeInTheDocument();
    expect(getDetail).toHaveBeenCalledTimes(1);
  });

  it("does not replace another selected pull request when route detail resolves", async () => {
    const user = userEvent.setup();
    const routeDetail =
      deferred<
        Awaited<ReturnType<ApiClient["getCodeDeliveryPullRequestDetail"]>>
      >();
    const getDetail = vi.fn(({ number }: { number: number }) =>
      number === 2251
        ? routeDetail.promise
        : Promise.resolve(deliveryPullRequestDetails[number]!),
    );
    const client = deliveryClient({
      getCodeDeliveryPullRequestDetail: getDetail as never,
    });
    await renderDeliveryRoute(
      "pull_requests",
      client,
      "/code/delivery/pull-requests?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&pr=2251",
    );

    await user.click(await rowFor("Make workspace deep links durable"));
    expect(
      await screen.findByRole("heading", {
        name: "Make workspace deep links durable",
      }),
    ).toBeInTheDocument();

    routeDetail.resolve(deliveryPullRequestDetails[2251]!);
    await new Promise((resolve) => window.setTimeout(resolve, 20));
    expect(
      screen.getByRole("heading", {
        name: "Make workspace deep links durable",
      }),
    ).toBeInTheDocument();
    expect(
      getDetail.mock.calls.filter(([request]) => request.number === 2251),
    ).toHaveLength(1);
  });

  it("keeps exact pull-request detail open when the aggregate list fails", async () => {
    const list =
      deferred<
        Awaited<ReturnType<ApiClient["queryCodeDeliveryPullRequests"]>>
      >();
    const client = deliveryClient({
      queryCodeDeliveryPullRequests: () => list.promise,
    });
    await renderDeliveryRoute(
      "pull_requests",
      client,
      "/code/delivery/pull-requests?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&pr=2251",
    );

    expect(
      await screen.findByRole("heading", { name: "Build the delivery center" }),
    ).toBeInTheDocument();
    list.reject(new Error("aggregate unavailable"));

    expect(
      await screen.findByText("aggregate unavailable"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Build the delivery center" }),
    ).toBeInTheDocument();
  });
});

describe("delivery run list", () => {
  it("accepts load, refreshed detail, and mutation completions in Strict Mode", async () => {
    vi.mocked(toast.success).mockClear();
    const baseDetail = deliveryRunDetails[4401]!;
    const refreshedDetail = {
      ...baseDetail,
      errors: [
        {
          kind: "detail",
          message: "Strict Mode run refresh completed.",
        },
      ],
    };
    const mutatedDetail = {
      ...baseDetail,
      errors: [
        {
          kind: "detail",
          message: "Strict Mode rerun refresh completed.",
        },
      ],
    };
    let phase: "load" | "mutation" = "load";
    const getDetail = vi.fn(async () =>
      phase === "mutation" ? mutatedDetail : baseDetail,
    );
    const rerun = vi.fn(async () => ({
      success: true,
      message: "Rerun queued.",
    }));
    const onChanged = vi.fn();
    const client = deliveryClient({
      getCodeDeliveryRunDetail: getDetail,
      runCodeDeliveryRunAction: rerun,
    });
    const panel = (initialDetail?: typeof baseDetail) => (
      <StrictMode>
        <AppContextProvider value={appContext(client)}>
          <RunDetailSheet
            summary={deliveryRuns.find((item) => item.github_id === 4401)!}
            initialDetail={initialDetail}
            onClose={vi.fn()}
            onChanged={onChanged}
            onOpenWorkspace={vi.fn()}
          />
        </AppContextProvider>
      </StrictMode>
    );
    const view = render(panel());

    expect(await screen.findByText("Jobs")).toBeInTheDocument();
    view.rerender(panel(refreshedDetail));
    expect(
      await screen.findByText("Strict Mode run refresh completed."),
    ).toBeInTheDocument();

    phase = "mutation";
    await userEvent.click(screen.getByRole("button", { name: "Rerun failed" }));
    expect(
      await screen.findByText("Strict Mode rerun refresh completed."),
    ).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalledTimes(1);
    expect(rerun).toHaveBeenCalledWith(
      expect.objectContaining({ action: { type: "rerun_failed" } }),
    );
    expect(toast.success).toHaveBeenCalledWith("Rerun queued.");
  });

  it("reruns every job in a completed workflow run", async () => {
    const rerun = vi.fn(async () => ({
      success: true,
      message: "Workflow queued again.",
    }));
    const client = deliveryClient({ runCodeDeliveryRunAction: rerun });
    render(
      <AppContextProvider value={appContext(client)}>
        <RunDetailSheet
          summary={deliveryRuns.find((item) => item.github_id === 4401)!}
          initialDetail={deliveryRunDetails[4401]!}
          onClose={vi.fn()}
          onChanged={vi.fn()}
          onOpenWorkspace={vi.fn()}
        />
      </AppContextProvider>,
    );

    await userEvent.click(screen.getByRole("button", { name: "Rerun all" }));

    await waitFor(() =>
      expect(rerun).toHaveBeenCalledWith(
        expect.objectContaining({ action: { type: "rerun" } }),
      ),
    );
  });

  it("loads a changed run target within the same repository", async () => {
    const getDetail = vi.fn(async ({ id }: { id: number }) =>
      Promise.resolve(deliveryRunDetails[id]!),
    );
    const client = deliveryClient({
      queryCodeDeliveryRuns: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: [],
        errors: [],
        fetched_at: "2026-08-20T15:20:00.000Z",
      }),
      getCodeDeliveryRunDetail: getDetail as never,
    });
    const router = await renderDeliveryRoute(
      "runs",
      client,
      "/code/delivery/runs?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&runKind=workflow_run&runId=4401",
    );
    expect(
      await screen.findByRole("heading", { name: "Desktop CI" }),
    ).toBeInTheDocument();

    await router.navigate({
      to: "/code/delivery/runs",
      search: {
        repoHost: "github.com",
        repoOwner: "brightwave-inc",
        repoName: "tidebreak",
        runKind: "deployment",
        runId: 901,
      },
    });

    expect(
      await screen.findByRole("heading", { name: "Production" }),
    ).toBeInTheDocument();
    expect(getDetail.mock.calls.map(([target]) => target.id)).toEqual(
      expect.arrayContaining([4401, 901]),
    );
  });

  it("reuses a pending run detail request when the target row is selected", async () => {
    const user = userEvent.setup();
    const detail =
      deferred<Awaited<ReturnType<ApiClient["getCodeDeliveryRunDetail"]>>>();
    const getDetail = vi.fn(() => detail.promise);
    const client = deliveryClient({
      getCodeDeliveryRunDetail: getDetail,
    });
    await renderDeliveryRoute(
      "runs",
      client,
      "/code/delivery/runs?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&runKind=workflow_run&runId=4401",
    );

    await user.click(await rowFor("Desktop CI"));
    await new Promise((resolve) => window.setTimeout(resolve, 20));
    expect(getDetail).toHaveBeenCalledTimes(1);

    detail.resolve(deliveryRunDetails[4401]!);
    expect(
      await screen.findByRole("heading", { name: "Jobs" }),
    ).toBeInTheDocument();
    expect(getDetail).toHaveBeenCalledTimes(1);
  });

  it("does not replace another selected run when route detail resolves", async () => {
    const user = userEvent.setup();
    const routeDetail =
      deferred<Awaited<ReturnType<ApiClient["getCodeDeliveryRunDetail"]>>>();
    const getDetail = vi.fn(({ id }: { id: number }) =>
      id === 4401
        ? routeDetail.promise
        : Promise.resolve(deliveryRunDetails[id]!),
    );
    const client = deliveryClient({
      getCodeDeliveryRunDetail: getDetail as never,
    });
    await renderDeliveryRoute(
      "runs",
      client,
      "/code/delivery/runs?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&runKind=workflow_run&runId=4401",
    );

    await user.click(await rowFor("Production"));
    expect(
      await screen.findByRole("heading", { name: "Production" }),
    ).toBeInTheDocument();

    routeDetail.resolve(deliveryRunDetails[4401]!);
    await new Promise((resolve) => window.setTimeout(resolve, 20));
    expect(
      screen.getByRole("heading", { name: "Production" }),
    ).toBeInTheDocument();
    expect(
      getDetail.mock.calls.filter(([request]) => request.id === 4401),
    ).toHaveLength(1);
  });

  it("keeps exact run detail and its errors open when the aggregate list fails", async () => {
    const list =
      deferred<Awaited<ReturnType<ApiClient["queryCodeDeliveryRuns"]>>>();
    const detail = {
      ...deliveryRunDetails[4401]!,
      jobs: [],
      can_rerun_failed: false,
      errors: [
        {
          kind: "detail",
          message: "Could not load jobs: HTTP 503",
        },
      ],
    };
    const client = deliveryClient({
      queryCodeDeliveryRuns: () => list.promise,
      getCodeDeliveryRunDetail: async () => detail,
    });
    await renderDeliveryRoute(
      "runs",
      client,
      "/code/delivery/runs?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&runKind=workflow_run&runId=4401",
    );

    expect(
      await screen.findByRole("heading", { name: "Desktop CI" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Could not load jobs: HTTP 503"),
    ).toBeInTheDocument();
    list.reject(new Error("run aggregate unavailable"));

    expect(
      await screen.findByText("run aggregate unavailable"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Desktop CI" }),
    ).toBeInTheDocument();
  });

  it("ignores a rerun completion after the selected run changes", async () => {
    vi.mocked(toast.success).mockClear();
    const rerun =
      deferred<Awaited<ReturnType<ApiClient["runCodeDeliveryRunAction"]>>>();
    const queryRuns = vi.fn(async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: [],
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }));
    const client = deliveryClient({
      queryCodeDeliveryRuns: queryRuns,
      runCodeDeliveryRunAction: () => rerun.promise,
    });
    const router = await renderDeliveryRoute(
      "runs",
      client,
      "/code/delivery/runs?repoHost=github.com&repoOwner=brightwave-inc&repoName=tidebreak&runKind=workflow_run&runId=4401",
    );

    await userEvent.click(
      await screen.findByRole("button", { name: "Rerun failed" }),
    );
    await router.navigate({
      to: "/code/delivery/runs",
      search: {
        repoHost: "github.com",
        repoOwner: "brightwave-inc",
        repoName: "tidebreak",
        runKind: "deployment",
        runId: 901,
      },
    });
    expect(
      await screen.findByRole("heading", { name: "Production" }),
    ).toBeInTheDocument();
    const callsBeforeCompletion = queryRuns.mock.calls.length;

    rerun.resolve({ success: true, message: "Rerun queued." });
    await rerun.promise;
    await new Promise((resolve) => window.setTimeout(resolve, 0));
    expect(queryRuns).toHaveBeenCalledTimes(callsBeforeCompletion);
    expect(toast.success).not.toHaveBeenCalled();
    expect(
      screen.getByRole("heading", { name: "Production" }),
    ).toBeInTheDocument();
  });
});

describe("stack lanes", () => {
  it("indents children under their parent and badges an unloaded parent", async () => {
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: stackedDeliveryPullRequests,
        errors: [],
        fetched_at: "2026-08-21T15:00:00.000Z",
      }),
    } as unknown as ApiClient);

    await screen.findByText("Stack base: extract the fact store");
    const list = screen.getByRole("list", { name: "Pull requests" });
    const depths = [...list.querySelectorAll("[data-depth]")].map((row) =>
      row.getAttribute("data-depth"),
    );
    expect(depths).toEqual(["0", "1", "2", "0"]);
    expect(screen.getByText("Stacked on #2288")).toBeTruthy();
  });

  it("marks every member of a chain the host has no stack for", async () => {
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: unregisteredDeliveryPullRequests,
        errors: [],
        fetched_at: "2026-08-21T15:00:00.000Z",
      }),
    } as unknown as ApiClient);

    await screen.findByText("Unregistered base: land the schema");
    // All three members carry the marker, and the host-registered fixtures
    // above never do.
    expect(screen.getAllByText("Unregistered stack")).toHaveLength(3);
  });

  it("keeps stack lanes after opening detail without stack enrichment", async () => {
    const user = userEvent.setup();
    const detail = deliveryPullRequestDetails[2302]!;
    renderList({
      ...storyClient(),
      queryCodeDeliveryPullRequests: async () => ({
        capability: deliveryRepositoriesSnapshot.capability,
        items: stackedDeliveryPullRequests,
        errors: [],
        fetched_at: "2026-08-21T15:00:00.000Z",
      }),
      getCodeDeliveryPullRequestDetail: async () => ({
        ...detail,
        summary: {
          ...detail.summary,
          stack_parent_number: undefined,
          stack_number: undefined,
          stack_size: undefined,
        },
        stack: undefined,
      }),
    } as unknown as ApiClient);

    await user.click(await screen.findByText("Stack middle: reconcile sweep"));
    await screen.findByTestId("pull-request-detail-pane");
    await waitFor(() => {
      const list = screen.getByRole("list", { name: "Pull requests" });
      expect(
        [...list.querySelectorAll("[data-depth]")].map((row) =>
          row.getAttribute("data-depth"),
        ),
      ).toEqual(["0", "1", "2", "0"]);
    });
    expect(screen.getByText("Stacked on #2288")).toBeInTheDocument();
  });
});
