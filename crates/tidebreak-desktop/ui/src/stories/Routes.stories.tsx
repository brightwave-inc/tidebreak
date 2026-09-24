import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, within } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { ApiClient, ManagedPolicy } from "@/api";
import type { AppContextValue } from "@/AppContext";
import { AppsPage } from "@/apps/AppsPage";
import { HomeRoute } from "@/HomeRoute";
import { InboxView } from "@/InboxView";
import { PluginsPage } from "@/plugins/PluginsPage";
import { ProjectFilesView } from "@/ProjectFilesView";
import { SETTINGS_SECTIONS } from "@/settings/sections";
import { SettingsRoute } from "@/SettingsRoute";
import { WorkLayout } from "@/WorkLayout";
import {
  RouteCrashScreen,
  RouteNotFound,
  RoutePaneError,
} from "@/RouteFallbacks";
import {
  denseInboxEntries,
  managedPolicy,
  pending,
  resetRouteStoryStores,
  routeProjectDocuments,
  routeProjectInstructions,
  routeProjects,
  RouteStoryProviders,
  storyClient,
  unmanagedPolicy,
} from "./routeStoryHarness";
import { storyModels, storyProviders } from "./SettingsStoryHarness";
import { failureFixtures } from "./fixtures";

type RouteScenario =
  | "home"
  | "home-no-provider"
  | "home-managed-no-model"
  | "home-project"
  | "inbox-loading"
  | "inbox-empty"
  | "inbox-dense"
  | "project-loading"
  | "project-empty"
  | "project-failure"
  | "project-dense"
  | "project-instructions"
  | "settings-unmanaged"
  | "settings-instructions"
  | "settings-managed"
  | "settings-connected-apps"
  | "settings-notifications"
  | "apps-list"
  | "apps-detail"
  | "plugins-list"
  | "plugins-detail"
  | "route-crash"
  | "shell-crash"
  | "not-found"
  | "settings-not-found";

function InboxRouteComposition() {
  return (
    <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
      <InboxView />
    </div>
  );
}

/** A page whose render throws, the way a bad payload crashes a real one. */
function BrokenPage(): never {
  throw new TypeError("Cannot read properties of undefined (reading 'title')");
}

function createRouteRouter(
  initialPath: string,
  { shellCrash = false }: { shellCrash?: boolean } = {},
) {
  // The app's shell has no rail of its own; each layout draws one. A shell
  // that crashes takes the window, so this story's shell can too.
  const rootRoute = createRootRoute({
    component: shellCrash
      ? () => {
          throw new Error("The shell could not read its saved window layout.");
        }
      : undefined,
  });
  // The app's shape: every Work route hangs off one layout that mounts the
  // rail once.
  const workLayoutRoute = createRoute({
    getParentRoute: () => rootRoute,
    id: "work-layout",
    component: WorkLayout,
  });
  const homeRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/",
    validateSearch: (search: Record<string, unknown>) => ({
      project: typeof search.project === "string" ? search.project : undefined,
    }),
    component: () => {
      const { project } = homeRoute.useSearch();
      return <HomeRoute key={project ?? "home"} projectId={project ?? null} />;
    },
  });
  const inboxRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/inbox",
    component: InboxRouteComposition,
  });
  const projectRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/p/$projectId",
    component: () => {
      const { projectId } = projectRoute.useParams();
      return (
        <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-auto">
          <ProjectFilesView projectId={projectId} />
        </div>
      );
    },
  });
  const appsRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/apps",
    component: () => <AppsPage />,
  });
  const appDetailRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/apps/$appId",
    component: () => {
      const { appId } = appDetailRoute.useParams();
      return <AppsPage appId={appId} />;
    },
  });
  const pluginsRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/plugins",
    component: () => <PluginsPage />,
  });
  const pluginDetailRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/plugins/$pluginId",
    component: () => {
      const { pluginId } = pluginDetailRoute.useParams();
      return <PluginsPage pluginId={pluginId} />;
    },
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: SettingsRoute,
  });
  const settingsSectionRoutes = SETTINGS_SECTIONS.map((section) =>
    createRoute({
      getParentRoute: () => settingsRoute,
      errorComponent: RoutePaneError,
      path: section.path,
      component: section.Component,
      ...(section.validateSearch
        ? { validateSearch: section.validateSearch }
        : {}),
    }),
  );

  const brokenRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    errorComponent: RoutePaneError,
    path: "/broken",
    component: BrokenPage,
  });

  return createRouter({
    routeTree: rootRoute.addChildren([
      workLayoutRoute.addChildren([
        homeRoute,
        inboxRoute,
        projectRoute,
        appsRoute,
        appDetailRoute,
        pluginsRoute,
        pluginDetailRoute,
        brokenRoute,
      ]),
      settingsRoute.addChildren(settingsSectionRoutes),
    ]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
    // The fallbacks `createAppRouter` configures, so these stories show the
    // screens the app shows.
    defaultErrorComponent: RouteCrashScreen,
    defaultNotFoundComponent: RouteNotFound,
  });
}

function clientForScenario(scenario: RouteScenario): ApiClient {
  if (scenario === "project-loading") {
    return storyClient({ listProjectDocuments: async () => pending() });
  }
  if (scenario === "project-failure") {
    return storyClient({
      listProjectDocuments: async () => {
        throw failureFixtures.unreachable;
      },
    });
  }
  if (scenario === "project-empty") {
    return storyClient({
      listProjectDocuments: async () => ({
        documents: [],
        next_cursor: null,
      }),
    });
  }
  if (scenario === "settings-instructions") {
    return storyClient({
      getPersonalInstructions: async () => ({
        instructions:
          "Answer in British English.\n\nLead with the answer, then the detail.",
      }),
    });
  }
  if (scenario === "project-dense") {
    return storyClient({
      listProjectDocuments: async () => ({
        documents: routeProjectDocuments,
        next_cursor: null,
      }),
    });
  }
  return storyClient();
}

function initialPathFor(scenario: RouteScenario): string {
  if (scenario === "home-project") return "/?project=project-1";
  if (scenario.startsWith("inbox")) return "/inbox";
  if (scenario === "project-instructions") return "/p/project-1#instructions";
  if (scenario.startsWith("project")) return "/p/project-1";
  if (scenario === "settings-instructions") return "/settings/instructions";
  if (scenario === "settings-managed") return "/settings/gateway";
  if (scenario === "settings-connected-apps") return "/settings/connected-apps";
  if (scenario === "settings-notifications") return "/settings/notifications";
  if (scenario === "settings-unmanaged") return "/settings/providers";
  if (scenario === "apps-list") return "/apps";
  if (scenario === "apps-detail") return "/apps/release-brief";
  if (scenario === "plugins-list") return "/plugins";
  if (scenario === "plugins-detail") return "/plugins/document-work";
  if (scenario === "route-crash") return "/broken";
  if (scenario === "not-found") return "/c/chat-that-moved/files";
  if (scenario === "settings-not-found") return "/settings/retired-section";
  return "/";
}

function policyFor(scenario: RouteScenario): ManagedPolicy {
  return scenario === "settings-managed" || scenario === "home-managed-no-model"
    ? managedPolicy
    : unmanagedPolicy;
}

/**
 * A fresh install: the catalog loaded, and nothing in it can run because no
 * provider is connected yet.
 */
function contextFor(
  scenario: RouteScenario,
): Partial<AppContextValue> | undefined {
  if (scenario === "home-no-provider") {
    return {
      models: storyModels.map((model) => ({ ...model, available: false })),
      providers: storyProviders.map((provider) => ({
        ...provider,
        enabled: false,
        has_credential: false,
      })),
      catalogLoaded: true,
    };
  }
  if (scenario === "home-managed-no-model") {
    return { models: [], providers: [], catalogLoaded: true };
  }
  return undefined;
}

function RoutesStory({ scenario }: { scenario: RouteScenario }) {
  const [state] = useState(() => {
    const inboxLoading = scenario === "inbox-loading";
    const inboxDense = scenario === "inbox-dense";
    resetRouteStoryStores({
      inboxEntries: inboxDense ? denseInboxEntries : [],
      inboxLoaded: !inboxLoading,
      attentionChatIds: inboxDense ? ["chat-2"] : [],
      projects:
        scenario === "project-instructions"
          ? routeProjects.map((project) =>
              project.id === "project-1"
                ? { ...project, instructions: routeProjectInstructions }
                : project,
            )
          : routeProjects,
    });
    return {
      client: clientForScenario(scenario),
      policy: policyFor(scenario),
      context: contextFor(scenario),
      router: createRouteRouter(initialPathFor(scenario), {
        shellCrash: scenario === "shell-crash",
      }),
    };
  });

  return (
    <RouteStoryProviders
      client={state.client}
      policy={state.policy}
      context={state.context}
    >
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </RouteStoryProviders>
  );
}

const meta = {
  title: "Navigation/Routes",
  component: RoutesStory,
  args: { scenario: "home" },
  parameters: { layout: "fullscreen" },
  render: (args) => <RoutesStory key={args.scenario} {...args} />,
} satisfies Meta<typeof RoutesStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Home at 1280 × 800: two-column starter cards, full intro. */
export const HomeDesktop: Story = {};

/** Home at 720 × 480: compact starter rows so every opener sits above the composer. */
export const HomeMinimumWindow: Story = {
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/**
 * A fresh install with nothing connected: the setup card stands where the
 * starters go, and the composer says why it cannot send.
 */
export const HomeNoProvider: Story = {
  args: { scenario: "home-no-provider" },
};

/** The same first run in the 420 px compact pane. */
export const HomeNoProviderCompact: Story = {
  args: { scenario: "home-no-provider" },
  globals: { viewport: { value: "compact", isRotated: false } },
};

/** The same first run at the 720 × 480 minimum window. */
export const HomeNoProviderMinimumWindow: Story = {
  args: { scenario: "home-no-provider" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/**
 * A managed profile whose gateway has nothing for this person yet: there is
 * no key to add, so home points at the gateway instead.
 */
export const HomeManagedNoModel: Story = {
  args: { scenario: "home-managed-no-model" },
};

/**
 * New work started from a project: the composer names the project the work
 * will be filed in, and the conversation waits for the first message.
 */
export const HomeInProject: Story = {
  args: { scenario: "home-project" },
};

export const HomeInProjectMinimumWindow: Story = {
  args: { scenario: "home-project" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

export const InboxLoading: Story = {
  args: { scenario: "inbox-loading" },
};

export const InboxEmpty: Story = {
  args: { scenario: "inbox-empty" },
};

export const InboxDense: Story = {
  args: { scenario: "inbox-dense" },
};

export const InboxDenseMinimumWindow: Story = {
  args: { scenario: "inbox-dense" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

export const ProjectFilesLoading: Story = {
  args: { scenario: "project-loading" },
};

export const ProjectFilesEmpty: Story = {
  args: { scenario: "project-empty" },
};

export const ProjectFilesFailure: Story = {
  args: { scenario: "project-failure" },
};

export const ProjectFilesDense: Story = {
  args: { scenario: "project-dense" },
};

export const ProjectFilesDenseMinimumWindow: Story = {
  args: { scenario: "project-dense" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/** Opened from the project menu's Instructions item: the brief is in focus. */
export const ProjectInstructions: Story = {
  args: { scenario: "project-instructions" },
};

export const ProjectInstructionsMinimumWindow: Story = {
  args: { scenario: "project-instructions" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

export const SettingsInstructions: Story = {
  args: { scenario: "settings-instructions" },
};

/** Notifications in the settings rail, after Appearance. */
export const SettingsNotifications: Story = {
  args: { scenario: "settings-notifications" },
};

export const SettingsProvidersUnmanaged: Story = {
  args: { scenario: "settings-unmanaged" },
};

export const SettingsModelGatewayManaged: Story = {
  args: { scenario: "settings-managed" },
};

export const SettingsConnectedApps: Story = {
  args: { scenario: "settings-connected-apps" },
  globals: { viewport: { value: "desktop", isRotated: false } },
};

export const SettingsConnectedAppsMinimumWindow: Story = {
  args: { scenario: "settings-connected-apps" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

export const AppsRegisteredList: Story = {
  args: { scenario: "apps-list" },
};

export const AppsRegisteredDetail: Story = {
  args: { scenario: "apps-detail" },
};

export const PluginsRegisteredList: Story = {
  args: { scenario: "plugins-list" },
};

export const PluginsRegisteredDetail: Story = {
  args: { scenario: "plugins-detail" },
};

/**
 * A page that crashed inside the Work frame: the pane says so, and the rail
 * beside it keeps working.
 */
export const RouteCrash: Story = {
  args: { scenario: "route-crash" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("heading", {
        name: "This page hit an unexpected error",
      }),
    ).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Try again" }),
    ).toBeVisible();
  },
};

export const RouteCrashMinimumWindow: Story = {
  args: { scenario: "route-crash" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/** A crash in the shell itself, where no rail survived: it takes the window. */
export const ShellCrash: Story = {
  args: { scenario: "shell-crash" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("heading", {
        name: "Tidebreak hit an unexpected error.",
      }),
    ).toBeVisible();
    for (const name of ["Reload", "Go home", "Copy debug info"]) {
      await expect(canvas.getByRole("button", { name })).toBeVisible();
    }
  },
};

/** An address no route answers, under the Work rail, with a way home. */
export const NotFound: Story = {
  args: { scenario: "not-found" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByRole("heading", { name: "This page does not exist" }),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Go home" })).toBeVisible();
  },
};

export const NotFoundMinimumWindow: Story = {
  args: { scenario: "not-found" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
};

/** An unknown settings section fills the settings pane and keeps its rail. */
export const SettingsNotFound: Story = {
  args: { scenario: "settings-not-found" },
};
