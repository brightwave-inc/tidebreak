// @vitest-environment jsdom
import { StrictMode } from "react";
import { cleanup, render, screen, within } from "@testing-library/react";
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
  deliveryRepositoriesSnapshot,
  deliveryRunDetails,
  deliveryRuns,
} from "../stories/fixtures";
import {
  CodeDeliveryPage,
  RunDetailPanel,
  codeDeliverySearchFrom,
} from "./CodeDeliveryPage";
import { useCodeDeliveryStore } from "./CodeDeliveryStore";

afterEach(() => {
  cleanup();
  useCodeDeliveryStore.getState().reset();
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

  it("still reports the review state on a live pull request", async () => {
    renderList();
    expect(
      within(await rowFor("Build the delivery center")).getByText(
        "Changes requested",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByText(
        "Approved",
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

  it("keeps the ready-to-merge and attention marks apart", async () => {
    renderList();
    expect(
      within(await rowFor("Make workspace deep links durable")).getByLabelText(
        "Ready to merge",
      ),
    ).toBeInTheDocument();
    expect(
      within(await rowFor("Build the delivery center")).getByLabelText(
        "Needs attention",
      ),
    ).toBeInTheDocument();
    // A merged pull request is neither.
    const merged = await rowFor("Cache the workspace digest between polls");
    expect(within(merged).queryByLabelText("Ready to merge")).toBeNull();
    expect(within(merged).queryByLabelText("Needs attention")).toBeNull();
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
          <RunDetailPanel
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
    expect(toast.success).toHaveBeenCalledWith("Rerun queued.");
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
