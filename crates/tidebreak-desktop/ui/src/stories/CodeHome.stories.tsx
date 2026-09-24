import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, fireEvent, userEvent, waitFor, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type {
  CodeSessionDigest,
  CodeUpdateNotice,
  CodeWorkspaceSnapshot,
  HarnessKind,
} from "@/api/types";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { CodeHome } from "@/code/CodeHome";
import { CodeLayout } from "@/code/CodeLayout";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { useInbox } from "@/Inbox";
import { useNotifications } from "@/NotificationStore";
import { useUiStore } from "@/UiStore";
import {
  codeHomeBusySessions,
  codeHomeBusyWorkspaces,
  codeHomeQuietSessions,
  codeHomeQuietWorkspaces,
  codeRepositories,
  harnessDoctor,
  harnessDoctorCold,
  harnessDoctorSignedOut,
} from "./fixtures";

/**
 * The Code home inside the code layout, so the rail and the page read the
 * same catalog and the same live snapshot, at the width the app gives them.
 */
type HomeScenario =
  | "busy"
  | "quiet"
  | "no-workspaces"
  | "empty"
  | "loading"
  | "failure"
  | "needs-harness"
  | "fresh-machine";

function pending<T>(): Promise<T> {
  return new Promise(() => {});
}

function storySocket(): WebSocket {
  return {
    close: () => {},
    onclose: null,
    onerror: null,
    onmessage: null,
    onopen: null,
  } as unknown as WebSocket;
}

function scenarioWork(scenario: HomeScenario): {
  workspaces: CodeWorkspaceSnapshot[];
  sessions: CodeSessionDigest[];
} {
  if (scenario === "busy") {
    return {
      workspaces: codeHomeBusyWorkspaces,
      sessions: codeHomeBusySessions,
    };
  }
  if (scenario === "quiet") {
    return {
      workspaces: codeHomeQuietWorkspaces,
      sessions: codeHomeQuietSessions,
    };
  }
  return { workspaces: [], sessions: [] };
}

function storyClient(scenario: HomeScenario): ApiClient {
  const loading = scenario === "loading";
  const failure = scenario === "failure";
  const repos =
    scenario === "empty" ||
    scenario === "needs-harness" ||
    scenario === "fresh-machine"
      ? []
      : codeRepositories;
  const { workspaces, sessions } = scenarioWork(scenario);
  const doctor =
    scenario === "needs-harness"
      ? harnessDoctorSignedOut
      : scenario === "fresh-machine"
        ? harnessDoctorCold
        : harnessDoctor;

  return {
    listCodeRepos: async () => {
      if (loading) return pending();
      if (failure) throw new Error("The repository catalog could not load.");
      return repos;
    },
    listCodeWorkspaces: async () => {
      if (loading) return pending();
      if (failure) throw new Error("The repository catalog could not load.");
      return workspaces;
    },
    getHarnessDoctor: async () => (loading ? pending() : doctor),
    refreshHarnessDoctor: async () => doctor,
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
    listCodeGithubRepositories: async () => ({ repositories: [] }),
    getCodeRepoSources: async () => ({
      sources: [
        { kind: "local", available: true },
        { kind: "git_url", available: true },
        { kind: "github", available: true },
      ],
      chooses_destination: false,
    }),
    // The live channel restates its snapshot on connect, as the server does.
    openCodeUpdates: (onNotice: (notice: CodeUpdateNotice) => void) => {
      if (!loading) {
        queueMicrotask(() => onNotice({ type: "snapshot", sessions }));
      }
      return storySocket();
    },
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
    togglePinChat: () => {},
    archiveChat: () => {},
    unarchiveChat: () => {},
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

function Placeholder({ label }: { label: string }) {
  return (
    <div className="content-container grid min-h-0 flex-1 place-items-center p-8">
      <p className="text-sm text-muted-foreground">{label}</p>
    </div>
  );
}

function storyRouter() {
  const rootRoute = createRootRoute();
  const codeLayout = createRoute({
    getParentRoute: () => rootRoute,
    id: "code-layout",
    component: CodeLayout,
  });
  const codeChild = (path: string, label: string) =>
    createRoute({
      getParentRoute: () => codeLayout,
      path,
      component: () => <Placeholder label={label} />,
    });
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
        createRoute({
          getParentRoute: () => codeLayout,
          path: "/code",
          component: CodeHome,
        }),
        codeChild("/code/inbox", "Inbox"),
        codeChild("/code/w/$workspaceId", "Workspace"),
        codeChild("/code/s/$sessionId", "Conversation"),
        codeChild("/code/delivery/pull-requests", "Pull requests"),
        codeChild("/code/analytics", "Analytics"),
        codeChild("/code/archive", "Archive"),
      ]),
    ]),
    history: createMemoryHistory({ initialEntries: ["/code"] }),
  });
}

function resetStoryState(): void {
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
  useInbox.setState({ entries: [], loaded: true });
}

function CodeHomeStory({ scenario }: { scenario: HomeScenario }) {
  const [state] = useState(() => {
    resetStoryState();
    return { client: storyClient(scenario), router: storyRouter() };
  });

  useEffect(
    () => () => {
      disconnectCodeUpdates();
      useCodeCatalogStore.getState().reset();
    },
    [],
  );

  return (
    <AppContextProvider value={appContext(state.client)}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Home",
  component: CodeHomeStory,
  args: { scenario: "busy" },
  parameters: { layout: "fullscreen" },
  render: (args) => <CodeHomeStory key={args.scenario} {...args} />,
} satisfies Meta<typeof CodeHomeStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/**
 * A returning reader with a lot going on. Needs you holds eight items, so it
 * shows five and offers View all; running work, ready pull requests, and
 * recent work follow, with the repositories last.
 */
export const Busy: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await canvas.findByRole("heading", { name: /Needs you/ });
  },
};

/**
 * The same page with Needs you opened to all eight items. Focus lands on the
 * first row View all revealed.
 */
export const BusyExpanded: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "View all 8 in Needs you" }),
    );
    await expect(
      canvas.getByRole("button", { name: "Show fewer in Needs you" }),
    ).toHaveAttribute("aria-expanded", "true");
  },
};

/** The smallest window the app allows, with the rail open beside the home. */
export const BusyMinimumWindow: Story = {
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/** Nothing waits on the reader: the page says so, then shows recent work. */
export const Quiet: Story = {
  args: { scenario: "quiet" },
  play: async ({ canvasElement }) => {
    await within(canvasElement).findByText("Nothing needs you right now.");
  },
};

/** Repositories are registered but no workspace is open on any of them. */
export const NoWorkspaces: Story = {
  args: { scenario: "no-workspaces" },
};

/** The repository row menu: right-click, Shift+F10, or the menu key. */
export const RepositoryMenu: Story = {
  args: { scenario: "quiet" },
  play: async ({ canvasElement }) => {
    const row = await within(canvasElement).findByRole("button", {
      name: "New workspace on tidebreak",
    });
    row.scrollIntoView({ block: "center" });
    const bounds = row.getBoundingClientRect();
    fireEvent.contextMenu(row, {
      clientX: bounds.left + 120,
      clientY: bounds.top + bounds.height / 2,
    });
    await within(document.body).findByRole("menuitem", {
      name: "Repository settings…",
    });
  },
};

/**
 * The repository hover card: details and the two common actions. It opens
 * beside the row when the window has room for it there, as the rail's card
 * does, and otherwise hangs below the row's far end, clear of the names.
 */
export const RepositoryDetails: Story = {
  args: { scenario: "quiet" },
  play: async ({ canvasElement }) => {
    const row = await within(canvasElement).findByRole("button", {
      name: "New workspace on model-gateway",
    });
    // A short window starts the list below the fold; hover it where a
    // reader would, on screen.
    row.scrollIntoView({ block: "center" });
    await userEvent.hover(row);
    await waitFor(
      () =>
        expect(
          within(document.body).getByTestId("repository-hover-card"),
        ).toBeVisible(),
      { timeout: 3000 },
    );
  },
};

export const FirstRepository: Story = {
  args: { scenario: "empty" },
};

export const Loading: Story = { args: { scenario: "loading" } };

export const Failure: Story = {
  args: { scenario: "failure" },
};

/** Engines downloaded but none signed in: each row offers Sign in. */
export const NeedsHarness: Story = {
  args: { scenario: "needs-harness" },
};

/**
 * Nothing downloaded yet. The doctor holds the page, because a download
 * alone does not sign an engine in.
 */
export const FreshMachine: Story = {
  args: { scenario: "fresh-machine" },
};

export const CompactFirstRepository: Story = {
  args: { scenario: "empty" },
  globals: { viewport: { value: "compact", isRotated: false } },
};
