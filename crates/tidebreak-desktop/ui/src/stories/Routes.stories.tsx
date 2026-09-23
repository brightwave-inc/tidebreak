import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import type { ApiClient, ManagedPolicy } from "@/api";
import { AppsPage } from "@/apps/AppsPage";
import { HomeRoute } from "@/HomeRoute";
import { InboxView } from "@/InboxView";
import { PluginsPage } from "@/plugins/PluginsPage";
import { ProjectFilesView } from "@/ProjectFilesView";
import { SETTINGS_SECTIONS } from "@/settings/sections";
import { SettingsRoute } from "@/SettingsRoute";
import { WorkLayout } from "@/WorkLayout";
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

type RouteScenario =
  | "home"
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
  | "plugins-detail";

function InboxRouteComposition() {
  return (
    <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
      <InboxView />
    </div>
  );
}

function createRouteRouter(initialPath: string) {
  const rootRoute = createRootRoute();
  // The app's shape: every Work route hangs off one layout that mounts the
  // rail once.
  const workLayoutRoute = createRoute({
    getParentRoute: () => rootRoute,
    id: "work-layout",
    component: WorkLayout,
  });
  const homeRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
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
    path: "/inbox",
    component: InboxRouteComposition,
  });
  const projectRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
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
    path: "/apps",
    component: () => <AppsPage />,
  });
  const appDetailRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    path: "/apps/$appId",
    component: () => {
      const { appId } = appDetailRoute.useParams();
      return <AppsPage appId={appId} />;
    },
  });
  const pluginsRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
    path: "/plugins",
    component: () => <PluginsPage />,
  });
  const pluginDetailRoute = createRoute({
    getParentRoute: () => workLayoutRoute,
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
      path: section.path,
      component: section.Component,
      ...(section.validateSearch
        ? { validateSearch: section.validateSearch }
        : {}),
    }),
  );

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
      ]),
      settingsRoute.addChildren(settingsSectionRoutes),
    ]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });
}

function clientForScenario(scenario: RouteScenario): ApiClient {
  if (scenario === "project-loading") {
    return storyClient({ listProjectDocuments: async () => pending() });
  }
  if (scenario === "project-failure") {
    return storyClient({
      listProjectDocuments: async () => {
        throw new Error("Project files could not be loaded from this machine.");
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
  return "/";
}

function policyFor(scenario: RouteScenario): ManagedPolicy {
  return scenario === "settings-managed" ? managedPolicy : unmanagedPolicy;
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
      router: createRouteRouter(initialPathFor(scenario)),
    };
  });

  return (
    <RouteStoryProviders client={state.client} policy={state.policy}>
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

export const HomeDesktop: Story = {};

export const HomeMinimumWindow: Story = {
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
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
