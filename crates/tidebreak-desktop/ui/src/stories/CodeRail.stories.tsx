import { useEffect, useState, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";

import type { InboxEntry } from "@/api";
import type { ApiClient } from "@/api/client";
import type { HarnessKind } from "@/api/types";
import { AppContextProvider } from "@/AppContext";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { CodeSidebar } from "@/code/CodeSidebar";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { useInbox } from "@/Inbox";
import { InboxView } from "@/InboxView";
import { useNotifications } from "@/NotificationStore";
import { RouteFrame } from "@/RouteFrame";
import { useUiStore } from "@/UiStore";
import {
  codeRepositories,
  codeSidebarWorkspaces,
  harnessDoctor,
} from "./fixtures";
import { denseInboxEntries, storyAppContext } from "./routeStoryHarness";

/**
 * The code rail with the Inbox at the top of its destinations.
 *
 * Opening the Inbox from code mode keeps this rail: the same inbox renders
 * under it at `/code/inbox`, so going to what waits does not drop you into
 * work mode.
 */
type RailScenario =
  | "waiting"
  | "nothing-waiting"
  | "inbox-open"
  | "inbox-empty";

const WAITING: InboxEntry[] = [
  denseInboxEntries[5]!,
  denseInboxEntries[0]!,
  denseInboxEntries[1]!,
];

function storySocket(): WebSocket {
  return {
    close: () => {},
    onclose: null,
    onerror: null,
    onmessage: null,
    onopen: null,
  } as unknown as WebSocket;
}

function storyClient(): ApiClient {
  return {
    listCodeRepos: async () => codeRepositories,
    listCodeWorkspaces: async () => codeSidebarWorkspaces,
    getHarnessDoctor: async () => harnessDoctor,
    refreshHarnessDoctor: async () => harnessDoctor,
    listCodeHarnessModels: async (kind: HarnessKind) => ({
      kind,
      models: [],
      reasoning_efforts: [],
    }),
    getCodeSubscriptionUsage: async () => ({
      source: "model_gateway",
      providers: [],
    }),
    getCodeCloneDefaults: async () => ({
      parent_dir: "/Users/sam/src",
      gh_found: true,
      gh_authenticated: true,
      gh_remediation: "",
    }),
    openCodeUpdates: storySocket,
    getGatewayStatus: async () => ({
      signed_in: false,
      model_count: 0,
      sign_in: { state: "idle" as const },
    }),
    getCodeDeliveryRepositories: async () => ({
      capability: {
        found: true,
        authenticated: true,
        viewer_login: "github",
        remediation: "",
      },
      repositories: [],
      errors: [],
      fetched_at: "2026-08-15T00:00:00.000Z",
    }),
    listNotifications: async () => ({ notifications: [], nextCursor: null }),
    notificationUnreadCount: async () => 0,
  } as unknown as ApiClient;
}

function Placeholder({ label }: { label: string }) {
  return (
    <div className="content-container grid min-h-0 flex-1 place-items-center p-8">
      <p className="text-sm text-muted-foreground">{label}</p>
    </div>
  );
}

function storyRouter(initialPath: string) {
  const rootRoute = createRootRoute();
  const codeLayout = createRoute({
    getParentRoute: () => rootRoute,
    id: "code-layout",
    component: () => (
      <RouteFrame sidebar={<CodeSidebar />}>
        <Outlet />
      </RouteFrame>
    ),
  });
  const codeChild = (path: string, component: () => ReactNode) =>
    createRoute({ getParentRoute: () => codeLayout, path, component });
  const stub = (path: string, label: string) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <Placeholder label={label} />,
    });

  return createRouter({
    routeTree: rootRoute.addChildren([
      stub("/", "Work"),
      stub("/inbox", "Work inbox"),
      stub("/settings", "Settings"),
      codeLayout.addChildren([
        codeChild("/code", () => <Placeholder label="Workspaces" />),
        codeChild("/code/inbox", () => (
          <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
            <InboxView />
          </div>
        )),
        codeChild("/code/w/$workspaceId", () => (
          <Placeholder label="Workspace" />
        )),
        codeChild("/code/delivery/pull-requests", () => (
          <Placeholder label="Pull requests" />
        )),
        codeChild("/code/analytics", () => <Placeholder label="Analytics" />),
        codeChild("/code/archive", () => <Placeholder label="Archive" />),
      ]),
    ]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });
}

function resetStoryState(scenario: RailScenario): void {
  disconnectCodeUpdates();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    railPrefs: DEFAULT_RAIL_PREFS,
    newWorkspaceOpen: false,
    addRepoOpen: false,
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  useUiStore.setState({ sidebarCollapsed: false, sidebarWidth: 280 });
  useNotifications.setState({ notifications: [], unread: 0, loaded: true });
  useInbox.setState({
    entries: scenario === "waiting" || scenario === "inbox-open" ? WAITING : [],
    loaded: true,
  });
}

function CodeRailStory({ scenario }: { scenario: RailScenario }) {
  const [state] = useState(() => {
    resetStoryState(scenario);
    const client = storyClient();
    return {
      client,
      router: storyRouter(
        scenario === "inbox-open" || scenario === "inbox-empty"
          ? "/code/inbox"
          : "/code",
      ),
    };
  });

  useEffect(
    () => () => {
      disconnectCodeUpdates();
      useCodeCatalogStore.getState().reset();
      useInbox.getState().clear();
    },
    [],
  );

  return (
    <AppContextProvider value={storyAppContext(state.client)}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Rail",
  component: CodeRailStory,
  args: { scenario: "waiting" },
  parameters: { layout: "fullscreen" },
  render: (args) => <CodeRailStory key={args.scenario} {...args} />,
} satisfies Meta<typeof CodeRailStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Three conversations wait on you: the Inbox leads the rail with a count. */
export const InboxWaiting: Story = {};

/** Nothing waits: the Inbox stays, without a count. */
export const NothingWaiting: Story = {
  args: { scenario: "nothing-waiting" },
};

/** The Inbox open from code mode, under the code rail. */
export const InboxOpen: Story = {
  args: { scenario: "inbox-open" },
};

export const InboxOpenEmpty: Story = {
  args: { scenario: "inbox-empty" },
};

/** The smallest window the app allows, with the rail open beside the inbox. */
export const InboxOpenMinimumWindow: Story = {
  args: { scenario: "inbox-open" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};
