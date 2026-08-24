import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import { expect, fn, userEvent, within } from "storybook/test";

import { RouteFrame } from "@/RouteFrame";
import { AppSidebar } from "@/sidebar/AppSidebar";
import { NewProjectDialog } from "@/sidebar/NewProjectDialog";
import { SidebarExpandStrip } from "@/sidebar/SidebarExpandStrip";
import {
  denseInboxEntries,
  denseRouteChats,
  resetRouteStoryStores,
  routeChats,
  RouteStoryProviders,
  storyClient,
} from "./routeStoryHarness";

type NavigationScenario =
  | "active-work"
  | "loading"
  | "empty"
  | "dense"
  | "failure"
  | "narrow"
  | "collapsed";

function NavigationSurface({ activeChatId }: { activeChatId?: string }) {
  const activeChat = routeChats.find((chat) => chat.id === activeChatId);
  return (
    <RouteFrame sidebar={<AppSidebar chat={activeChat} />}>
      <div className="content-container grid h-full min-h-0 place-items-center p-8">
        <div className="max-w-md text-center">
          <h1 className="text-2xl font-semibold tracking-tight">
            Navigation review surface
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            This canvas keeps the production rail in context while you inspect
            active routes, list density, loading, and failure states.
          </p>
        </div>
      </div>
    </RouteFrame>
  );
}

function createNavigationRouter(initialPath: string) {
  const rootRoute = createRootRoute();
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: NavigationSurface,
  });
  const inboxRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/inbox",
    component: NavigationSurface,
  });
  const appsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/apps",
    component: NavigationSurface,
  });
  const pluginsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/plugins",
    component: NavigationSurface,
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: NavigationSurface,
  });
  const codeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code",
    component: NavigationSurface,
  });
  const projectRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/p/$projectId",
    component: NavigationSurface,
  });
  const chatRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/c/$chatId",
    component: () => <NavigationSurface activeChatId="chat-1" />,
  });
  const projectChatRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/p/$projectId/c/$chatId",
    component: () => <NavigationSurface activeChatId="chat-3" />,
  });

  return createRouter({
    routeTree: rootRoute.addChildren([
      homeRoute,
      inboxRoute,
      appsRoute,
      pluginsRoute,
      settingsRoute,
      codeRoute,
      projectRoute,
      chatRoute,
      projectChatRoute,
    ]),
    history: createMemoryHistory({ initialEntries: [initialPath] }),
  });
}

function NavigationStory({ scenario }: { scenario: NavigationScenario }) {
  const [state] = useState(() => {
    const dense = scenario === "dense";
    resetRouteStoryStores({
      chats:
        scenario === "empty" || scenario === "loading"
          ? []
          : dense
            ? denseRouteChats
            : routeChats,
      chatsLoaded: scenario !== "loading",
      chatsError:
        scenario === "failure"
          ? "Work could not be loaded from this machine."
          : null,
      projects: scenario === "empty" || scenario === "loading" ? [] : undefined,
      projectsLoaded: scenario !== "loading",
      inboxEntries: dense ? denseInboxEntries : [],
      inboxLoaded: scenario !== "loading",
      attentionChatIds: dense ? ["chat-2", "dense-chat-2"] : ["chat-2"],
      sidebarWidth: scenario === "narrow" ? 220 : 280,
      sidebarCollapsed: scenario === "collapsed",
    });

    const initialPath =
      scenario === "active-work"
        ? "/c/chat-1"
        : scenario === "dense"
          ? "/inbox"
          : scenario === "narrow"
            ? "/apps"
            : scenario === "collapsed"
              ? "/apps"
              : "/";
    return {
      client: storyClient(),
      router: createNavigationRouter(initialPath),
    };
  });

  return (
    <RouteStoryProviders client={state.client}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <SidebarExpandStrip macOverlay={false} />
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </RouteStoryProviders>
  );
}

const meta = {
  title: "Navigation/Sidebar",
  component: NavigationStory,
  args: { scenario: "active-work" },
  parameters: { layout: "fullscreen" },
  render: (args) => <NavigationStory key={args.scenario} {...args} />,
} satisfies Meta<typeof NavigationStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ActiveWork: Story = {};

export const LoadingLists: Story = {
  args: { scenario: "loading" },
};

export const EmptyLists: Story = {
  args: { scenario: "empty" },
};

export const DenseLists: Story = {
  args: { scenario: "dense" },
};

export const ChatLoadFailure: Story = {
  args: { scenario: "failure" },
};

export const NarrowRail: Story = {
  args: { scenario: "narrow" },
  parameters: {
    docs: {
      description: {
        story:
          "The rail stays fully featured at its narrower saved width. This replaces the misleading compact-rail name; compact now refers only to viewport stories.",
      },
    },
  },
};

export const CollapsedRail: Story = {
  args: { scenario: "collapsed" },
};

export const CollapseAndRestoreActiveRoute: Story = {
  args: { scenario: "narrow" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const appsLink = await canvas.findByRole("button", { name: "Apps" });
    await expect(appsLink).toHaveAttribute("aria-current", "page");

    await userEvent.click(
      canvas.getByRole("button", { name: "Collapse sidebar" }),
    );
    await userEvent.click(
      await canvas.findByRole("button", { name: "Expand sidebar" }),
    );

    await expect(
      await canvas.findByRole("button", { name: "Apps" }),
    ).toHaveAttribute("aria-current", "page");
  },
};

export const NewProject: Story = {
  render: () => (
    <NewProjectDialog
      open
      creating={false}
      onOpenChange={fn()}
      onCreate={fn(async () => true)}
    />
  ),
};

export const CreatingProject: Story = {
  render: () => (
    <NewProjectDialog
      open
      creating
      onOpenChange={fn()}
      onCreate={fn(async () => true)}
    />
  ),
};
