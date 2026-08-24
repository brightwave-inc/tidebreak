import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, within } from "storybook/test";

import type { ApiClient, ManagedPolicy } from "@/api";
import { AppsPage } from "@/apps/AppsPage";
import { HomeRoute } from "@/HomeRoute";
import { InboxView } from "@/InboxView";
import { PluginsPage } from "@/plugins/PluginsPage";
import { ProjectFilesView } from "@/ProjectFilesView";
import { RouteFrame } from "@/RouteFrame";
import { SETTINGS_SECTIONS } from "@/settings/sections";
import { SettingsRoute } from "@/SettingsRoute";
import { AppSidebar } from "@/sidebar/AppSidebar";
import {
  denseInboxEntries,
  managedPolicy,
  pending,
  resetRouteStoryStores,
  routeProjectDocuments,
  RouteStoryProviders,
  storyClient,
  unmanagedPolicy,
} from "./routeStoryHarness";

type RouteScenario =
  | "home"
  | "inbox-loading"
  | "inbox-empty"
  | "inbox-dense"
  | "project-loading"
  | "project-empty"
  | "project-failure"
  | "project-dense"
  | "settings-unmanaged"
  | "settings-managed"
  | "settings-connected-apps"
  | "apps-list"
  | "apps-detail"
  | "plugins-list"
  | "plugins-detail";

function InboxRouteComposition() {
  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <InboxView />
      </div>
    </RouteFrame>
  );
}

function createRouteRouter(initialPath: string) {
  const rootRoute = createRootRoute();
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: HomeRoute,
  });
  const inboxRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/inbox",
    component: InboxRouteComposition,
  });
  const projectRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/p/$projectId",
    component: () => {
      const { projectId } = projectRoute.useParams();
      return (
        <RouteFrame sidebar={<AppSidebar />}>
          <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-auto">
            <ProjectFilesView projectId={projectId} />
          </div>
        </RouteFrame>
      );
    },
  });
  const appsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/apps",
    component: () => <AppsPage />,
  });
  const appDetailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/apps/$appId",
    component: () => {
      const { appId } = appDetailRoute.useParams();
      return <AppsPage appId={appId} />;
    },
  });
  const pluginsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/plugins",
    component: () => <PluginsPage />,
  });
  const pluginDetailRoute = createRoute({
    getParentRoute: () => rootRoute,
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
      homeRoute,
      inboxRoute,
      projectRoute,
      appsRoute,
      appDetailRoute,
      pluginsRoute,
      pluginDetailRoute,
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
  if (scenario.startsWith("inbox")) return "/inbox";
  if (scenario.startsWith("project")) return "/p/project-1";
  if (scenario === "settings-managed") return "/settings/gateway";
  if (scenario === "settings-connected-apps") return "/settings/connected-apps";
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

async function expectSettingsRouteLayout(canvasElement: HTMLElement) {
  const canvas = within(canvasElement);
  const activeItem = await canvas.findByRole("button", {
    name: "Connected apps",
  });
  await expect(activeItem).toHaveAttribute("aria-current", "page");

  const sidebar = canvasElement.querySelector<HTMLElement>(".settings-sidebar");
  const main = canvasElement.querySelector<HTMLElement>(".settings-main");
  await expect(sidebar).toBeVisible();
  await expect(main).toBeVisible();

  const sidebarRect = sidebar?.getBoundingClientRect();
  const mainRect = main?.getBoundingClientRect();
  await expect(
    Math.abs((sidebarRect?.top ?? 0) - (mainRect?.top ?? 0)),
  ).toBeLessThan(2);
  await expect(mainRect?.left ?? 0).toBeGreaterThanOrEqual(
    (sidebarRect?.right ?? 0) - 1,
  );
  await expect(mainRect?.width ?? 0).toBeGreaterThan(400);
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

export const HomeDesktop: Story = {
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).findByRole("heading", { name: "How can I help?" }),
    ).resolves.toBeTruthy();
  },
};

export const HomeMinimumWindow: Story = {
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

export const SettingsProvidersUnmanaged: Story = {
  args: { scenario: "settings-unmanaged" },
};

export const SettingsModelGatewayManaged: Story = {
  args: { scenario: "settings-managed" },
};

export const SettingsConnectedApps: Story = {
  args: { scenario: "settings-connected-apps" },
  globals: { viewport: { value: "desktop", isRotated: false } },
  play: async ({ canvasElement }) => {
    await expectSettingsRouteLayout(canvasElement);
  },
};

export const SettingsConnectedAppsMinimumWindow: Story = {
  args: { scenario: "settings-connected-apps" },
  globals: { viewport: { value: "minimumWindow", isRotated: false } },
  play: async ({ canvasElement }) => {
    await expectSettingsRouteLayout(canvasElement);
  },
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
