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
  | "pull-requests-partial"
  | "github-unavailable"
  | "runs"
  | "archive"
  | "archive-empty"
  | "notifications"
  | "notifications-empty";

function pending<T>(): Promise<T> {
  return new Promise(() => {});
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
  const workspaces =
    scenario === "archive-empty"
      ? deliveryWorkspaces.filter(
          (workspace) => workspace.status !== "archived",
        )
      : deliveryWorkspaces;

  return {
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
    queryCodeDeliveryRuns: async () => ({
      capability: deliveryRepositoriesSnapshot.capability,
      items: deliveryRuns,
      errors: [],
      fetched_at: "2026-08-20T15:20:00.000Z",
    }),
    getCodeDeliveryRunDetail: async ({ id }: CodeDeliveryRunTarget) =>
      deliveryRunDetails[id] ?? deliveryRunDetails[4401]!,
    runCodeDeliveryRunAction: async () => ({
      success: true,
      message: "Failed jobs queued.",
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
    await expect(await canvas.findAllByText("Approved")).toHaveLength(1);
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

export const PullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByText("Make workspace deep links durable"),
    );
    await expect(
      await canvas.findByRole("heading", {
        name: "Make workspace deep links durable",
      }),
    ).toBeVisible();
  },
};

/** The full GitHub-shaped panel: lifecycle, diffstat, reviewers, Markdown. */
export const PullRequestDetailConversation: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByText("Build the delivery center"));
    await expect(
      await canvas.findByRole("heading", { name: "Build the delivery center" }),
    ).toBeVisible();
    // The description renders as Markdown rather than raw "## Summary" text.
    await expect(
      await canvas.findByRole("heading", { name: "Summary" }),
    ).toBeVisible();
    await expect(await canvas.findByText("+2140")).toBeVisible();
    await expect(
      await canvas.findByRole("button", { name: /Comment/ }),
    ).toBeVisible();
  },
};

export const PullRequestDetailFiles: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByText("Build the delivery center"));
    await userEvent.click(await canvas.findByRole("tab", { name: /Files/ }));
    await expect(await canvas.findByText(/19 files changed/)).toBeVisible();
    await userEvent.click(
      await canvas.findByTitle(
        "crates/tidebreak-desktop/ui/src/code/pullRequestPresentation.ts",
      ),
    );
    await expect(await canvas.findByText(/@@ -0,0 \+1,8 @@/)).toBeVisible();
  },
};

export const PullRequestDetailChecks: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByText("Build the delivery center"));
    await userEvent.click(await canvas.findByRole("tab", { name: /Checks/ }));
    await expect(await canvas.findByText(/1 of 2 passed/)).toBeVisible();
  },
};

/** Merged: no merge controls, a reopen-free panel, and who merged it. */
export const MergedPullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByText("Cache the workspace digest between polls"),
    );
    await expect(
      await canvas.findByRole("heading", {
        name: "Cache the workspace digest between polls",
      }),
    ).toBeVisible();
    await expect(await canvas.findByText(/merged .* by devon/)).toBeVisible();
    await expect(
      canvas.queryByRole("button", { name: "Merge" }),
    ).not.toBeInTheDocument();
  },
};

/** Closed without merging: the only action left is to reopen it. */
export const ClosedPullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByText("Rewrite the deployment runbook"),
    );
    await expect(
      await canvas.findByRole("button", { name: "Reopen" }),
    ).toBeVisible();
  },
};

/** Draft: mark ready is offered, merge is not. */
export const DraftPullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByText("Document managed deployments"),
    );
    await expect(
      await canvas.findByRole("button", { name: "Mark ready" }),
    ).toBeVisible();
    await expect(
      await canvas.findByText("No description provided."),
    ).toBeVisible();
  },
};

/** Conflicting: the merge button says why it is disabled. */
export const BlockedMergePullRequestDetail: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByText("Adopt the shared status tone map"),
    );
    await expect(
      await canvas.findByText(
        "Resolve the conflicts with the base branch first.",
      ),
    ).toBeVisible();
    await expect(
      await canvas.findByRole("button", { name: "Merge" }),
    ).toBeDisabled();
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
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByText("Desktop CI"));
    await expect(
      await canvas.findByRole("heading", { name: "Desktop CI" }),
    ).toBeVisible();
    await expect(
      await canvas.findByText("Build static Storybook"),
    ).toBeVisible();
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
    const canvas = within(canvasElement);
    await userEvent.click(await canvas.findByText("Build the delivery center"));
    await expect(
      await canvas.findByRole("heading", { name: "Build the delivery center" }),
    ).toBeVisible();
  },
};
