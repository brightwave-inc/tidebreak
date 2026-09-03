import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
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
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { panelSearchFrom } from "@/panel/panelUrl";
import { useUiStore } from "@/UiStore";
import {
  deliveryCodeRepo,
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
  | "pull-requests-stacked-auto-merge-unavailable"
  | "pull-requests-unregistered"
  | "pull-requests-state-changed"
  | "pull-requests-partial"
  | "pull-requests-no-viewer"
  | "github-unavailable"
  | "runs"
  | "archive"
  | "archive-search"
  | "archive-empty";

/**
 * Open one pull request's detail sheet from the list.
 *
 * Scoped to the list on purpose: a workspace in the rail can carry the same
 * title as the pull request it opened, and an unscoped text query matches
 * the rail first — which navigates to the workspace instead of opening the
 * sheet, and the story then asserts against a page that is not there.
 */

/** The same, for the runs and deployments list. */

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
          (workspace) => workspace.status !== "released",
        )
      : deliveryWorkspaces;
  const refreshedMergedPullRequest = {
    ...deliveryPullRequests[0]!,
    state: "merged" as const,
    merged_at: "2026-08-27T19:45:00.000Z",
    updated_at: "2026-08-27T19:45:00.000Z",
  };
  const stackedAutoMergeUnavailable = stackedDeliveryPullRequests.map((item) =>
    item.number === 2302
      ? {
          ...item,
          checks: [{ name: "desktop UI", bucket: "pending" as const }],
          ready_to_merge: false,
          mergeable: "unknown",
          merge_state_status: "blocked",
        }
      : item,
  );

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
              : scenario === "pull-requests-stacked-auto-merge-unavailable"
                ? stackedAutoMergeUnavailable
                : scenario === "pull-requests-unregistered"
                  ? unregisteredDeliveryPullRequests
                  : scenario === "pull-requests-state-changed"
                    ? [deliveryPullRequests[0]!]
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
      scenario === "pull-requests-state-changed" && number === 2251
        ? {
            ...deliveryPullRequestDetails[2251]!,
            summary: refreshedMergedPullRequest,
          }
        : scenario === "pull-requests-stacked-auto-merge-unavailable" &&
            number === 2302
          ? {
              ...deliveryPullRequestDetails[2302]!,
              summary: {
                ...stackedAutoMergeUnavailable.find(
                  (item) => item.number === 2302,
                )!,
                stack_parent_number: undefined,
                stack_number: undefined,
                stack_size: undefined,
              },
              stack: undefined,
            }
          : (deliveryPullRequestDetails[number] ??
            deliveryPullRequestDetails[2251]!),
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
    searchCodeWorkspace: async (
      _workspaceId: string,
      query: Parameters<ApiClient["searchCodeWorkspace"]>[1],
    ) => ({
      matches: [],
      ...(scenario === "archive-search" &&
      query.history &&
      query.query.toLocaleLowerCase().includes("reclaim")
        ? {
            history_matches: [
              {
                workspace_id: "ws-archived-shortcuts",
                workspace_title: "Unify keyboard shortcuts",
                session_id: "session-reclaim-notes",
                turn_id: "turn-reclaim-notes",
                source: "turn_user_input" as const,
                preview:
                  "Make the reclaim tiers safe by keeping archived conversations searchable.",
                created_at: "2026-08-12T09:15:00.000Z",
              },
            ],
          }
        : {}),
      truncated: false,
    }),
    startCodeWatch: async () => ({}) as never,
    patchCodeWorkspace: async () => deliveryWorkspaces[0]!,
    archiveCodeWorkspace: async () => ({
      ...deliveryWorkspaces[0]!,
      status: "released",
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
    validateSearch: (search: Record<string, unknown>) =>
      panelSearchFrom(search),
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
  return createRouter({
    routeTree: rootRoute.addChildren([
      homeRoute,
      codeRoute,
      settingsRoute,
      workspaceRoute,
      pullRequestsRoute,
      runsRoute,
      archiveRoute,
    ]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });
}

function resetStoryState(_scenario: DeliveryScenario): void {
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
export const PullRequestAttentionGroups: Story = {};

/** A running check stays blue and in Waiting instead of becoming a failure. */
export const PullRequestRunningCheck: Story = {};

/** A running check on a merge-queue repo offers merge when ready, not a blocked Merge. */
export const PullRequestRunningCheckDetail: Story = {};

/** Auto-merge and queue membership live on the PR mark and in one handoff group. */
export const PullRequestMergeHandoffs: Story = {};

/** Repository grouping makes a many-repository queue scannable by ownership. */
export const PullRequestsByRepository: Story = {};

/**
 * The list carries every lifecycle at once. A merged or closed row used to
 * read "Review Pending", because GitHub drops the review decision the moment
 * a pull request settles.
 */
export const PullRequestLifecycles: Story = {};

/**
 * Stack lanes (decision 77): children indent under their parent in fact
 * order, and a child whose parent is not loaded stays flat with a
 * "stacked on" chip instead of a hidden edge.
 */
export const PullRequestStacks: Story = {
  args: { scenario: "pull-requests-stacked" },
};

/**
 * The stacked pull request's detail sheet: the host stack map pins the chain
 * bottom to top, and the merge offer is the whole stack rather than the one
 * layer — the chain lands every open layer in order.
 */
export const PullRequestStackDetail: Story = {
  args: { scenario: "pull-requests-stacked" },
};

/**
 * Detail hydration may omit optional stack enrichment. The list keeps its
 * lane, and a stacked pull request with pending checks offers no unsupported
 * GitHub auto-merge action.
 */
export const PullRequestStackDetailWithoutAutoMerge: Story = {
  args: { scenario: "pull-requests-stacked-auto-merge-unavailable" },
};

export const PullRequestDetail: Story = {};

/** Opening stale list data adopts the merged state and moves the row to Done. */
export const PullRequestStateChangedOnOpen: Story = {
  args: { scenario: "pull-requests-state-changed" },
};

/**
 * A stack-shaped chain the host has no stack for: every row carries the
 * unregistered marker, and the detail sheet offers to register the chain
 * so GitHub owns the ordering — instead of the reader merging a layer into
 * the branch below it by accident.
 */
export const PullRequestUnregisteredStack: Story = {
  args: { scenario: "pull-requests-unregistered" },
};

/** The full GitHub-shaped sheet: lifecycle, diffstat, reviewers, Markdown. */
export const PullRequestDetailConversation: Story = {};

/**
 * Newest first is the default: the reason a reader opens a busy pull request
 * is the latest verdict. The select flips back to the host's chronology.
 */
export const PullRequestCommentOrdering: Story = {};

/**
 * Admin merge stays behind the overflow and an inline confirmation: it
 * bypasses the branch protection that is otherwise disabling the plain merge
 * button. Inline rather than a dialog — the sheet is already a modal, and a
 * second stacked modal shares its dismiss layer.
 */
export const PullRequestAdminMerge: Story = {};

/**
 * A pull request with a linked active workspace routes its chores there; the
 * menu names the workspace before anything starts.
 */
export const PullRequestAgentActions: Story = {};

/**
 * Without a linked workspace the same menu cuts a fresh one from the pull
 * request's head branch, queues the prompt, and lands on the new workspace.
 */
export const PullRequestFreshAgent: Story = {};

export const PullRequestDetailFiles: Story = {};

export const PullRequestDetailChecks: Story = {};

/** Skipped checks are terminal, so a settled run reads complete in the tab. */
export const PullRequestTerminalChecks: Story = {};

/** Merged: no merge controls, a reopen-free sheet, and who merged it. */
export const MergedPullRequestDetail: Story = {};

/** Closed without merging: the only action left is to reopen it. */
export const ClosedPullRequestDetail: Story = {};

/** Draft: mark ready is offered, merge is not. */
export const DraftPullRequestDetail: Story = {};

/** Conflicting: no host merge action, and the card says why. */
export const BlockedMergePullRequestDetail: Story = {};

/**
 * The default view: your own open pull requests, drafts included. The author
 * comes from the login `gh` is signed in as, so "Yours" needs no typing.
 */
export const PullRequestsYours: Story = {};

/**
 * No login to filter on, so "Yours" is not offered and Delivery opens on the
 * attention view instead of quietly showing everybody's pull requests.
 */
export const PullRequestsWithoutViewerLogin: Story = {
  args: { scenario: "pull-requests-no-viewer" },
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
};

export const PullRequestKeyboard: Story = {};

export const ArchivePopulated: Story = {
  args: {
    scenario: "archive",
    initialUrl: "/code/archive",
  },
};

export const ArchiveConversationSearch: Story = {
  args: {
    scenario: "archive-search",
    initialUrl: "/code/archive",
  },
};

export const ArchiveEmpty: Story = {
  args: {
    scenario: "archive-empty",
    initialUrl: "/code/archive",
  },
};

export const NarrowPullRequestDetail: Story = {
  parameters: { viewport: { defaultViewport: "compact" } },
};

/**
 * The author filter is a lookup over logins Delivery has seen — avatars and
 * checkboxes — with typing kept as the fallback for a login it has not.
 */
export const PullRequestAuthorFilter: Story = {};
