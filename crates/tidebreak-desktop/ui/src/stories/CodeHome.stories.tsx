import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { HarnessKind } from "@/api/types";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { CodeHome } from "@/code/CodeHome";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { useUiStore } from "@/UiStore";
import {
  codeRepositories,
  codeSidebarWorkspaces,
  harnessDoctor,
  harnessDoctorDegraded,
} from "./fixtures";

type HomeScenario =
  | "registered"
  | "empty"
  | "loading"
  | "failure"
  | "needs-harness"
  | "dense-sidebar";

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

function storyClient(scenario: HomeScenario): ApiClient {
  const loading = scenario === "loading";
  const failure = scenario === "failure";
  const repos =
    scenario === "empty" || scenario === "needs-harness"
      ? []
      : codeRepositories;
  const workspaces = scenario === "dense-sidebar" ? codeSidebarWorkspaces : [];
  const doctor =
    scenario === "needs-harness" ? harnessDoctorDegraded : harnessDoctor;

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
      diagnostics: [],
      providers: [],
    }),
    getCodeCloneDefaults: async () => ({
      parent_dir: "/Users/sam/src",
      gh_found: true,
      gh_authenticated: true,
      gh_remediation: "",
    }),
    getCodeRepoSources: async () => ({
      sources: [
        { kind: "local", available: true },
        { kind: "git_url", available: true },
        { kind: "github", available: true },
      ],
      chooses_destination: false,
    }),
    openCodeUpdates: storySocket,
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

function storyRouter() {
  const rootRoute = createRootRoute();
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: CodeHome,
  });
  const stub = (path: string, label: string) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <p className="p-6">{label}</p>,
    });

  return createRouter({
    routeTree: rootRoute.addChildren([
      stub("/", "Work"),
      codeRoute,
      stub("/settings", "Settings"),
      stub("/code/w/$workspaceId", "Workspace"),
      stub("/code/delivery/pull-requests", "Delivery"),
      stub("/code/archive", "Archive"),
      stub("/code/notifications", "Notifications"),
    ]),
    history: createMemoryHistory({ initialEntries: ["/code"] }),
  });
}

function resetStoryState(scenario: HomeScenario): void {
  disconnectCodeUpdates();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    railPrefs:
      scenario === "dense-sidebar"
        ? { ...DEFAULT_RAIL_PREFS, density: "compact" }
        : DEFAULT_RAIL_PREFS,
    newWorkspaceOpen: false,
    addRepoOpen: false,
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  useUiStore.setState({ sidebarCollapsed: false, sidebarWidth: 280 });
}

function CodeHomeStory({ scenario }: { scenario: HomeScenario }) {
  const [state] = useState(() => {
    resetStoryState(scenario);
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
        <RouterProvider router={state.router as never} />
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Home",
  component: CodeHomeStory,
  args: { scenario: "registered" },
  parameters: { layout: "fullscreen" },
  render: (args) => <CodeHomeStory key={args.scenario} {...args} />,
} satisfies Meta<typeof CodeHomeStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const RegisteredRepositories: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("heading", { name: "Repos" }),
    ).toBeVisible();
    await expect(await canvas.findByText("model-gateway")).toBeVisible();
  },
};

export const FirstRepository: Story = {
  args: { scenario: "empty" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("heading", { name: "Start with a repository" }),
    ).toBeVisible();
  },
};

export const Loading: Story = { args: { scenario: "loading" } };

export const Failure: Story = {
  args: { scenario: "failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("The repository catalog could not load."),
    ).toBeVisible();
  },
};

export const NeedsHarness: Story = {
  args: { scenario: "needs-harness" },
};

export const DenseSidebar: Story = {
  args: { scenario: "dense-sidebar" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByText("Audit Storybook coverage"),
    ).toBeVisible();
    await expect(
      await canvas.findByText("Recover provider errors"),
    ).toBeVisible();
  },
};

export const CompactFirstRepository: Story = {
  args: { scenario: "empty" },
  parameters: { viewport: { defaultViewport: "compact" } },
};
