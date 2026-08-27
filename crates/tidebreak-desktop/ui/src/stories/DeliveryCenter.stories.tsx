import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, userEvent, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type {
  CodeDeliveryPullRequestAction,
  CodeDeliveryPullRequestActionBody,
  CodeDeliveryPullRequestTarget,
  CodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunActionBody,
  CodeDeliveryRunTarget,
  CodeWorkspaceSnapshot,
  HarnessKind,
} from "@/api/types";
import { CodeArchivePage } from "@/code/CodeArchivePage";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { CodeDeliveryPage } from "@/code/CodeDeliveryPage";
import { useCodeDeliveryStore } from "@/code/CodeDeliveryStore";
import { CodeNotificationsPage } from "@/code/CodeNotificationsPage";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { useUiStore } from "@/UiStore";
import {
  deliveryCodeRepo,
  deliveryNotifications,
  deliveryPullRequestDetails,
  deliveryPullRequests,
  stackedDeliveryPullRequests,
  unregisteredDeliveryPullRequests,
  deliveryRepositoriesSnapshot,
  deliveryRunDetails,
  deliveryRuns,
  deliveryWorkspaces,
  harnessDoctor,
} from "./fixtures";

type DeliveryScenario =
  | "pull-requests"
  | "pull-requests-loading"
  | "pull-requests-empty"
  | "pull-requests-stacked"
  | "pull-requests-unregistered"
  | "pull-requests-partial"
  | "pull-requests-no-viewer"
  | "github-unavailable"
  | "runs"
  | "archive"
  | "archive-empty"
  | "notifications"
  | "notifications-empty";

/**
 * Open one pull request's detail sheet from the list.
 *
 * Scoped to the list on purpose: a workspace in the rail can carry the same
 * title as the pull request it opened, and an unscoped text query matches
 * the rail first — which navigates to the workspace instead of opening the
 * sheet, and the story then asserts against a page that is not there.
 */
async function openPullRequest(canvasElement: HTMLElement, title: string) {
  await openListRow(canvasElement, "Pull requests", title);
}

/** The same, for the runs and deployments list. */
async function openRun(canvasElement: HTMLElement, title: string) {
  await openListRow(canvasElement, "Runs and deployments", title);
}

async function openListRow(
  canvasElement: HTMLElement,
  list: string,
  title: string,
) {
  const rows = within(canvasElement).getByRole("list", { name: list });
  await userEvent.click(await within(rows).findByText(title));
}

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

/**
 * A socket that opens and then says nothing.
 *
 * The rail subscribes to code updates on mount, so every Delivery story needs
 * one. Without it the page renders its error boundary instead of the list.
 */
function idleSocket(): WebSocket {
  const socket = {
    onopen: null as WebSocket["onopen"],
    onclose: null as WebSocket["onclose"],
    onerror: null as WebSocket["onerror"],
    close() {},
    addEventListener() {},
    removeEventListener() {},
  } as unknown as WebSocket;
  queueMicrotask(() => socket.onopen?.(new Event("open")));
  return socket;
}

function prActionMessage(action: CodeDeliveryPullRequestAction): string {
  switch (action.type) {
    case "rerun_failed":
      return "Failed checks queued.";
    case "mark_ready":
      return "Pull request marked ready.";
    case "close":
      return "Pull request closed.";
    case "reopen":
      return "Pull request reopened.";
    case "comment":
      return "Comment posted.";
    case "merge":
      return action.auto ? "Auto-merge enabled." : "Pull request merged.";
    case "create_stack":
      return "Stack registered on GitHub.";
  }
}

function storyClient(scenario: DeliveryScenario): ApiClient {
  const unavailableRepositories: CodeDeliveryRepositoriesSnapshot = {
    capability: {
      found: true,
      authenticated: false,
      remediation: "Run gh auth login, then refresh Delivery.",
    },
    repositories: [],
    errors: [],
    fetched_at: "2026-08-20T15:20:00.000Z",
  };
  // Signed in, but `gh` never said who: the old `gh auth status` has no
  // `--json`, so the login is missing while everything else works.
  const viewerlessRepositories: CodeDeliveryRepositoriesSnapshot = {
    ...deliveryRepositoriesSnapshot,
    capability: {
      found: true,
      authenticated: true,
      remediation: "",
    },
  };
  const workspaces =
    scenario === "archive-empty"
      ? deliveryWorkspaces.filter(
          (workspace) => workspace.status !== "archived",
        )
      : deliveryWorkspaces;

  return {
    openCodeUpdates: () => idleSocket(),
    listCodeRepos: async () => [deliveryCodeRepo],
    listCodeWorkspaces: async () => workspaces,
    getHarnessDoctor: async () => harnessDoctor,
    listCodeHarnessModels: async (kind: HarnessKind) => ({ kind, models: [] }),
    getCodeSubscriptionUsage: async () => ({
      source: "model_gateway",
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
              windows: [
                {
                  key: "weekly",
                  label: "Weekly",
                  used_percent: 64,
                  status: "allowed",
                },
              ],
            },
          ],
        },
      ],
    }),
    getCodeCloneDefaults: async () => ({
      gh_found: true,
      gh_authenticated: true,
      gh_remediation: "",
    }),
    getCodeDeliveryRepositories: async () =>
      scenario === "github-unavailable"
        ? unavailableRepositories
        : scenario === "pull-requests-no-viewer"
          ? viewerlessRepositories
          : deliveryRepositoriesSnapshot,
    resolveCodeDeliveryRepositories: async () => deliveryRepositoriesSnapshot,
    queryCodeDeliveryPullRequests: async () => {
      if (scenario === "pull-requests-loading") {
        return pending();
      }
      return {
        capability: deliveryRepositoriesSnapshot.capability,
        items:
          scenario === "pull-requests-empty"
            ? []
            : scenario === "pull-requests-stacked"
              ? stackedDeliveryPullRequests
              : scenario === "pull-requests-unregistered"
                ? unregisteredDeliveryPullRequests
                : deliveryPullRequests,
        errors:
          scenario === "pull-requests-partial"
            ? [
                {
                  repository: {
                    host: "github.com",
                    owner: "brightwave-inc",
                    name: "docs",
                  },
                  kind: "rate_limited",
                  message: "brightwave-inc/docs could not be refreshed yet.",
                  retry_at: "2026-08-20T15:35:00.000Z",
                },
              ]
            : [],
        fetched_at: "2026-08-20T15:20:00.000Z",
      };
    },
    getCodeDeliveryPullRequestDetail: async ({
      number,
    }: CodeDeliveryPullRequestTarget) =>
      deliveryPullRequestDetails[number] ?? deliveryPullRequestDetails[2251]!,
    runCodeDeliveryPullRequestAction: async ({
      action,
    }: CodeDeliveryPullRequestActionBody) => ({
      success: true,
      message: prActionMessage(action),
    }),
    createCodeWorkspace: async (body: {
      repo_id: string;
      title?: string;
      base_ref?: string;
    }) =>
      ({
        id: "ws-fresh-agent",
        repo_id: body.repo_id,
        title: body.title ?? "Fresh agent",
        worktree_path: "/Users/sam/tidebreak/worktrees/fresh-agent",
        branch_name: "thet/fresh-agent",
        base_ref: body.base_ref ?? "main",
        status: "active",
        created_at: "2026-08-20T15:25:00.000Z",
      }) as CodeWorkspaceSnapshot,
    writeCodeCheckLogs: async () => ({ logs: [], errors: [] }),
    queryCodeDeliveryRuns: async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryRuns,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }),
    getCodeDeliveryRunDetail: async ({ id }: CodeDeliveryRunTarget) =>
      deliveryRunDetails[id] ?? deliveryRunDetails[4401]!,
    runCodeDeliveryRunAction: async ({
      action,
    }: CodeDeliveryRunActionBody) => ({
      success: true,
      message:
        action.type === "rerun"
          ? "Workflow queued again."
          : "Failed jobs queued.",
    }),
    restoreCodeWorkspace: async (workspaceId: string) => {
      const workspace = deliveryWorkspaces.find(
        (candidate) => candidate.id === workspaceId,
      );
      return {
        ...(workspace ?? deliveryWorkspaces[0]!),
        status: "active",
      } as CodeWorkspaceSnapshot;
    },
    startCodeWatch: async () => ({}) as never,
    patchCodeWorkspace: async () => deliveryWorkspaces[0]!,
    archiveCodeWorkspace: async () => ({
      ...deliveryWorkspaces[0]!,
      status: "archived",
    }),
  } as unknown as ApiClient;
}

function appContext(client: ApiClient): AppContextValue {
  return {
    client,
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
}

function storyRouter(initialUrl: string) {
  const rootRoute = createRootRoute();
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => <p className="p-6">Work</p>,
  });
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: () => <p className="p-6">Code</p>,
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: () => <p className="p-6">Settings</p>,
  });
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    component: () => <p className="p-6">Workspace</p>,
  });
  const pullRequestsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/delivery/pull-requests",
    component: () => <CodeDeliveryPage surface="pull_requests" />,
  });
  const runsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/delivery/runs",
    component: () => <CodeDeliveryPage surface="runs" />,
  });
  const archiveRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/archive",
    component: CodeArchivePage,
  });
  const notificationsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/notifications",
    component: CodeNotificationsPage,
  });

  return createRouter({
    routeTree: rootRoute.addChildren([
      homeRoute,
      codeRoute,
      settingsRoute,
      workspaceRoute,
      pullRequestsRoute,
      runsRoute,
      archiveRoute,
      notificationsRoute,
    ]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
}

function resetStoryState(scenario: DeliveryScenario): void {
  disconnectCodeUpdates();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  useCodeDeliveryStore.getState().reset();
  useCodeUiStore.setState({
    railPrefs: DEFAULT_RAIL_PREFS,
    reviewSidebarOpen: false,
    newWorkspaceOpen: false,
    addRepoOpen: false,
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  useUiStore.setState({ sidebarCollapsed: false, sidebarWidth: 280 });
  // The author filter offers logins Delivery has already seen; seed the pool
  // the way a prior visit would have.
  useCodeDeliveryStore.setState({
    knownAuthors: [
      { login: "mara" },
      { login: "devon" },
      { login: "ines" },
      { login: "dependabot[bot]" },
    ],
  });

  if (scenario === "notifications") {
    useCodeDeliveryStore.setState({
      notifications: deliveryNotifications,
      seenFingerprints: Object.fromEntries(
        deliveryNotifications.map((notification) => [
          notification.fingerprint,
          notification.receivedAt,
        ]),
      ),
      lastPollAt: "2026-08-20T15:20:00.000Z",
      lastSuccessfulPollAt: "2026-08-20T15:20:00.000Z",
    });
  }
}

function DeliveryCenterStory({
  scenario,
  initialUrl,
}: {
  scenario: DeliveryScenario;
  initialUrl: string;
}) {
  const [state] = useState(() => {
    resetStoryState(scenario);
    const client = storyClient(scenario);
    return { client, router: storyRouter(initialUrl) };
  });

  useEffect(
    () => () => {
      useCodeCatalogStore.getState().reset();
      useCodeDeliveryStore.getState().reset();
      useCodeUpdatesStore.getState().reset();
    },
    [],
  );

  return (
    <AppContextProvider value={appContext(state.client)}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <RouterProvider router={state.router as never} />
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Delivery center",
  component: DeliveryCenterStory,
  args: {
    scenario: "pull-requests",
    initialUrl: "/code/delivery/pull-requests",
  },
  parameters: { layout: "fullscreen" },
  render: (args) => (
    <DeliveryCenterStory
      key={`${args.scenario}:${args.initialUrl}`}
      {...args}
    />
  ),
} satisfies Meta<typeof DeliveryCenterStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PullRequests: Story = {};

/**
 * The default grouping answers the page's main question: which pull requests
 * still need the reader, and which ones have already been handed to GitHub.
 */
export const PullRequestAttentionGroups: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(await canvas.findByText("Needs your attention")).toBeVisible();
    await expect(await canvas.findByText("Ready to merge")).toBeVisible();
    await expect(await canvas.findByText("Waiting")).toBeVisible();
    await expect(await canvas.findByText("Handed off")).toBeVisible();
  },
};

/** A running check stays blue and in Waiting instead of becoming a failure. */
export const PullRequestRunningCheck: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const title = await canvas.findByText(
      "Let the catalog say which scopes a server accepts",
    );
    const row = title.closest('[role="listitem"]');
    if (!(row instanceof HTMLElement)) {
      throw new Error("running-check row is missing");
    }
    await expect(row).toHaveAttribute("data-status-group", "waiting");
    await expect(within(row).getByText("Checks running")).toBeVisible();
    await expect(within(row).getByText("1 pending")).toBeVisible();
  },
};

/** A running check on a merge-queue repo offers merge when ready, not a blocked Merge. */
export const PullRequestRunningCheckDetail: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await canvas.findByText(
        "Let the catalog say which scopes a server accepts",
      ),
    );
    await expect(
      await body.findByRole("button", { name: "Merge when ready" }),
    ).toBeEnabled();
    await expect(body.queryByRole("button", { name: "Merge" })).toBeNull();
    await expect(
      body.queryByRole("button", { name: "Enable auto-merge" }),
    ).toBeNull();
    await expect(
      body.queryByText(/required review|review approval/i),
    ).not.toBeInTheDocument();
    await userEvent.click(await body.findByRole("tab", { name: /Checks/ }));
    await expect(
      await body.findByText("0 of 1 passed, 1 pending"),
    ).toBeVisible();
  },
};

/** Auto-merge and queue membership live on the PR mark and in one handoff group. */
export const PullRequestMergeHandoffs: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    for (const [titleText, statusText] of [
      ["Ask a hosted MCP server which scopes it accepts", "In merge queue"],
      ["Apply reasoning effort changes to the next turn", "Auto-merge armed"],
    ] as const) {
      const title = await canvas.findByText(titleText);
      const row = title.closest('[role="listitem"]');
      if (!(row instanceof HTMLElement)) {
        throw new Error(`${titleText} row is missing`);
      }
      await expect(row).toHaveAttribute("data-status-group", "handed_off");
      await expect(within(row).getByText(statusText)).toBeVisible();
    }
  },
};

/** Repository grouping makes a many-repository queue scannable by ownership. */
export const PullRequestsByRepository: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await canvas.findByRole("combobox", { name: "Group pull requests" }),
    );
    await userEvent.click(
      await body.findByRole("option", { name: "Group: repository" }),
    );
    await expect(
      canvasElement.querySelector(
        '[data-pull-request-group="brightwave-inc/tidebreak"]',
      ),
    ).not.toBeNull();
    await expect(
      canvasElement.querySelector(
        '[data-pull-request-group="brightwave-inc/model-gateway"]',
      ),
    ).not.toBeNull();
  },
};

/**
 * The list carries every lifecycle at once. A merged or closed row used to
 * read "Review Pending", because GitHub drops the review decision the moment
 * a pull request settles.
 */
export const PullRequestLifecycles: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("Cache the workspace digest between polls"),
    ).toBeVisible();
    // Two merged rows: one the host reports as MERGED, one it reports as
    // CLOSED with a merge timestamp.
    await expect(await canvas.findAllByText("Merged")).toHaveLength(2);
    await expect(await canvas.findAllByText("Closed")).toHaveLength(1);
    await expect(await canvas.findAllByText("Draft")).toHaveLength(1);
    await expect(await canvas.findAllByText("Ready to merge")).not.toHaveLength(
      0,
    );
    await expect(await canvas.findAllByText("Changes requested")).toHaveLength(
      1,
    );
  },
};

/**
 * Stack lanes (decision 62): children indent under their parent in fact
 * order, and a child whose parent is not loaded stays flat with a
 * "stacked on" chip instead of a hidden edge.
 */
export const PullRequestStacks: Story = {
  args: { scenario: "pull-requests-stacked" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("Stack base: extract the fact store"),
    ).toBeVisible();
    const rows = canvasElement.querySelectorAll("[data-depth]");
    const depths = [...rows].map((row) => row.getAttribute("data-depth"));
    await expect(depths).toEqual(["0", "1", "2", "0"]);
    await expect(await canvas.findByText("Stacked on #2288")).toBeVisible();
  },
};

/**
 * The stacked pull request's detail sheet: the host stack map pins the chain
 * bottom to top, and the merge offer is the whole stack rather than the one
 * layer — the chain lands every open layer in order.
 */
export const PullRequestStackDetail: Story = {
  args: { scenario: "pull-requests-stacked" },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Stack middle: reconcile sweep");
    await expect(
      await body.findByRole("heading", {
        name: "Stack middle: reconcile sweep",
      }),
    ).toBeVisible();
    // The stack map: bottom layer, this layer, the draft tip.
    await expect(await body.findByText("#2301")).toBeVisible();
    await expect(await body.findByText("#2303")).toBeVisible();
    await expect(await body.findByText("(this pull request)")).toBeVisible();
    await expect(
      await body.findByRole("button", { name: /Merge stack \(2 layers\)/ }),
    ).toBeVisible();
  },
};

export const PullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    // The detail is a sheet portaled to the document body, so its content
    // lives outside the story canvas.
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Make workspace deep links durable");
    await expect(
      await body.findByRole("heading", {
        name: "Make workspace deep links durable",
      }),
    ).toBeVisible();
  },
};

/**
 * A stack-shaped chain the host has no stack for: every row carries the
 * unregistered marker, and the detail sheet offers to register the chain
 * so GitHub owns the ordering — instead of the reader merging a layer into
 * the branch below it by accident.
 */
export const PullRequestUnregisteredStack: Story = {
  args: { scenario: "pull-requests-unregistered" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await expect(await canvas.findAllByText("Unregistered stack")).toHaveLength(
      3,
    );
    await openPullRequest(canvasElement, "Unregistered middle: the queries");
    await expect(
      await body.findByRole("button", { name: /Create stack \(3 layers\)/ }),
    ).toBeVisible();
    await userEvent.click(
      await body.findByRole("button", { name: /Create stack \(3 layers\)/ }),
    );
    await expect(
      await body.findByText(/Registers #2310, #2311, #2312/),
    ).toBeVisible();
  },
};

/** The full GitHub-shaped sheet: lifecycle, diffstat, reviewers, Markdown. */
export const PullRequestDetailConversation: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    await expect(
      await body.findByRole("heading", { name: "Build the delivery center" }),
    ).toBeVisible();
    // The description renders as Markdown rather than raw "## Summary" text.
    await expect(
      await body.findByRole("heading", { name: "Summary" }),
    ).toBeVisible();
    await expect(await body.findByText("+2140")).toBeVisible();
    await expect(
      await body.findByRole("button", { name: /Comment/ }),
    ).toBeVisible();
  },
};

/**
 * Newest first is the default: the reason a reader opens a busy pull request
 * is the latest verdict. The select flips back to the host's chronology.
 */
export const PullRequestCommentOrdering: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    const later = await body.findByText(
      "Keep repository failures visible without hiding usable results.",
    );
    const earlier = await body.findByText(/The narrow detail state still/);
    await expect(
      Boolean(
        later.compareDocumentPosition(earlier) &
          Node.DOCUMENT_POSITION_FOLLOWING,
      ),
    ).toBe(true);

    await userEvent.click(
      await body.findByRole("combobox", { name: "Comment order" }),
    );
    await userEvent.click(
      await body.findByRole("option", { name: "Oldest first" }),
    );
    await expect(
      Boolean(
        earlier.compareDocumentPosition(later) &
          Node.DOCUMENT_POSITION_FOLLOWING,
      ),
    ).toBe(true);
  },
};

/**
 * Admin merge stays behind the overflow and an inline confirmation: it
 * bypasses the branch protection that is otherwise disabling the plain merge
 * button. Inline rather than a dialog — the sheet is already a modal, and a
 * second stacked modal shares its dismiss layer.
 */
export const PullRequestAdminMerge: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    await expect(
      await body.findByRole("button", { name: "Enable auto-merge" }),
    ).toBeEnabled();
    await expect(body.queryByRole("button", { name: "Merge" })).toBeNull();
    await userEvent.click(
      await body.findByRole("button", { name: "More pull request actions" }),
    );
    await userEvent.click(
      await body.findByRole("menuitem", {
        name: /Admin merge \(bypass protections\)/,
      }),
    );
    await expect(
      await body.findByText(/skips any reviews and checks/),
    ).toBeVisible();
    await userEvent.click(
      await body.findByRole("button", { name: "Admin merge" }),
    );
  },
};

/**
 * A pull request with a linked active workspace routes its chores there; the
 * menu names the workspace before anything starts.
 */
export const PullRequestAgentActions: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    await userEvent.click(
      await body.findByRole("button", { name: "Fix with an agent" }),
    );
    await expect(
      await body.findByText(
        "Runs in Build the delivery center, the linked workspace.",
      ),
    ).toBeVisible();
    await expect(
      await body.findByRole("menuitem", { name: "Fix failing checks" }),
    ).toBeVisible();
    await expect(
      await body.findByRole("menuitem", { name: "Address review feedback" }),
    ).toBeVisible();
  },
};

/**
 * Without a linked workspace the same menu cuts a fresh one from the pull
 * request's head branch, queues the prompt, and lands on the new workspace.
 */
export const PullRequestFreshAgent: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Adopt the shared status tone map");
    await userEvent.click(
      await body.findByRole("button", { name: "Fix with an agent" }),
    );
    await expect(
      await body.findByText(
        "Starts a fresh workspace on brightwave-inc/tidebreak.",
      ),
    ).toBeVisible();
    await userEvent.click(
      await body.findByRole("menuitem", { name: "Resolve conflicts" }),
    );
    // The story router's workspace route is a stub; reaching it proves the
    // workspace was created and the sheet handed the reader over.
    await expect(await body.findByText("Workspace")).toBeVisible();
  },
};

export const PullRequestDetailFiles: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    await userEvent.click(await body.findByRole("tab", { name: /Files/ }));
    await expect(await body.findByText(/19 files changed/)).toBeVisible();
    await userEvent.click(
      await body.findByTitle(
        "crates/tidebreak-desktop/ui/src/code/pullRequestPresentation.ts",
      ),
    );
    await expect(await body.findByText(/@@ -0,0 \+1,8 @@/)).toBeVisible();
  },
};

export const PullRequestDetailChecks: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    await userEvent.click(await body.findByRole("tab", { name: /Checks/ }));
    await expect(await body.findByText(/1 of 2 passed/)).toBeVisible();
  },
};

/** Merged: no merge controls, a reopen-free sheet, and who merged it. */
export const MergedPullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(
      canvasElement,
      "Cache the workspace digest between polls",
    );
    await expect(
      await body.findByRole("heading", {
        name: "Cache the workspace digest between polls",
      }),
    ).toBeVisible();
    await expect(await body.findByText(/merged .* by devon/)).toBeVisible();
    await expect(
      body.queryByRole("button", { name: "Merge" }),
    ).not.toBeInTheDocument();
  },
};

/** Closed without merging: the only action left is to reopen it. */
export const ClosedPullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Rewrite the deployment runbook");
    await expect(
      await body.findByRole("button", { name: "Reopen" }),
    ).toBeVisible();
  },
};

/** Draft: mark ready is offered, merge is not. */
export const DraftPullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Document managed deployments");
    await expect(
      await body.findByRole("button", { name: "Mark ready" }),
    ).toBeVisible();
    await expect(
      await body.findByText("No description provided."),
    ).toBeVisible();
  },
};

/** Conflicting: no host merge action, and the card says why. */
export const BlockedMergePullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Adopt the shared status tone map");
    await expect(
      await body.findByText(
        "Resolve the conflicts with the base branch first.",
      ),
    ).toBeVisible();
    await expect(body.queryByRole("button", { name: "Merge" })).toBeNull();
    await expect(
      body.queryByRole("button", { name: "Enable auto-merge" }),
    ).toBeNull();
  },
};

/**
 * The default view: your own open pull requests, drafts included. The author
 * comes from the login `gh` is signed in as, so "Yours" needs no typing.
 */
export const PullRequestsYours: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("button", { name: "Yours", pressed: true }),
    ).toBeVisible();
  },
};

/**
 * No login to filter on, so "Yours" is not offered and Delivery opens on the
 * attention view instead of quietly showing everybody's pull requests.
 */
export const PullRequestsWithoutViewerLogin: Story = {
  args: { scenario: "pull-requests-no-viewer" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("button", { name: "Needs attention" }),
    ).toBeVisible();
    await expect(canvas.queryByRole("button", { name: "Yours" })).toBeNull();
  },
};

export const PullRequestsLoading: Story = {
  args: { scenario: "pull-requests-loading" },
};

export const PullRequestsEmpty: Story = {
  args: { scenario: "pull-requests-empty" },
};

export const PartialRepositoryFailure: Story = {
  args: { scenario: "pull-requests-partial" },
};

export const GitHubSignedOut: Story = {
  args: { scenario: "github-unavailable" },
};

export const RunsAndDeployments: Story = {
  args: {
    scenario: "runs",
    initialUrl: "/code/delivery/runs",
  },
};

export const RunDetail: Story = {
  args: {
    scenario: "runs",
    initialUrl: "/code/delivery/runs",
  },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openRun(canvasElement, "Desktop CI");
    await expect(
      await body.findByRole("heading", { name: "Desktop CI" }),
    ).toBeVisible();
    await expect(
      await body.findByRole("button", { name: "Rerun all" }),
    ).toBeVisible();
    await expect(
      await body.findByRole("button", { name: "Rerun failed" }),
    ).toBeVisible();
    await expect(await body.findByText("Build static Storybook")).toBeVisible();
  },
};

export const ArchivePopulated: Story = {
  args: {
    scenario: "archive",
    initialUrl: "/code/archive",
  },
};

export const ArchiveEmpty: Story = {
  args: {
    scenario: "archive-empty",
    initialUrl: "/code/archive",
  },
};

export const NotificationsFeed: Story = {
  args: {
    scenario: "notifications",
    initialUrl: "/code/notifications",
  },
};

export const NotificationsEmpty: Story = {
  args: {
    scenario: "notifications-empty",
    initialUrl: "/code/notifications",
  },
};

export const NotificationRules: Story = {
  args: {
    scenario: "notifications",
    initialUrl: "/code/notifications",
  },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByRole("tab", { name: "Rules" }));
    await expect(
      await canvas.findByRole("heading", {
        name: "Delivery notification rules",
      }),
    ).toBeVisible();
  },
};

export const NarrowPullRequestDetail: Story = {
  parameters: { viewport: { defaultViewport: "compact" } },
  play: async ({ canvasElement }) => {
    const body = within(canvasElement.ownerDocument.body);
    await openPullRequest(canvasElement, "Build the delivery center");
    await expect(
      await body.findByRole("heading", { name: "Build the delivery center" }),
    ).toBeVisible();
  },
};

/**
 * The author filter is a lookup over logins Delivery has seen — avatars and
 * checkboxes — with typing kept as the fallback for a login it has not.
 */
export const PullRequestAuthorFilter: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const body = within(canvasElement.ownerDocument.body);
    await userEvent.click(
      await canvas.findByRole("button", { name: /Filters/ }),
    );
    await expect(
      await body.findByRole("checkbox", { name: "mara" }),
    ).toBeVisible();
    await userEvent.click(await body.findByRole("checkbox", { name: "mara" }));
  },
};
