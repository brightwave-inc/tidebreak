import { useEffect } from "react";
import {
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
  useNavigate,
} from "@tanstack/react-router";

import { AppShell } from "./AppShell";
import { ChatExplorer } from "./ChatExplorer";
import { ChatRoute } from "./ChatRoute";
import { HomeRoute } from "./HomeRoute";
import { useManagedPolicy } from "./managedPolicy";
import type { PanelSearch } from "./panel/panelUrl";
import { RouteFrame } from "./RouteFrame";
import { SettingsRoute } from "./SettingsRoute";
import { defaultSettingsPathFor, SETTINGS_SECTIONS } from "./settings/sections";
import { HomeSidebar } from "./sidebar/HomeSidebar";

const rootRoute = createRootRoute({ component: AppShell });

const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: HomeRoute,
});

/**
 * The whole chat list as a searchable table. It shares home's rail — nothing
 * here is scoped to a conversation either.
 */
const allChatsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/chats",
  component: AllChatsRoute,
});

function AllChatsRoute() {
  return (
    <RouteFrame sidebar={<HomeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <ChatExplorer />
      </div>
    </RouteFrame>
  );
}

const chatRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/c/$chatId",
  validateSearch: (search: Record<string, unknown>): PanelSearch => ({
    left: typeof search.left === "string" ? search.left : undefined,
    right: typeof search.right === "string" ? search.right : undefined,
    fullscreen: typeof search.fullscreen === "string" ? search.fullscreen : undefined,
  }),
  component: ChatRouteComponent,
});

/**
 * Keyed on the chat id so a switch remounts rather than reusing the component.
 * Everything scoped to one conversation lives below here, and the unmount is
 * what guarantees none of it is carried into the next.
 */
function ChatRouteComponent() {
  const { chatId } = chatRoute.useParams();
  return <ChatRoute key={chatId} chatId={chatId} />;
}

export const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/settings",
  component: SettingsRoute,
});

/**
 * A bare `/settings` has no section of its own; send it to the first one this
 * profile has. Which that is depends on the resolved policy, which only exists
 * inside the tree the gate publishes it to — so the redirect is a component
 * rather than a `beforeLoad`.
 */
function SettingsIndexRedirect() {
  const navigate = useNavigate();
  const { managed } = useManagedPolicy();
  useEffect(() => {
    void navigate({ to: defaultSettingsPathFor(managed), replace: true });
  }, [managed, navigate]);
  // One frame, at most: a placeholder rather than nothing, so the settings
  // frame is never momentarily empty.
  return (
    <p className="text-muted-foreground p-6 text-sm">Opening settings…</p>
  );
}

const settingsIndexRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "/",
  component: SettingsIndexRedirect,
});

const settingsSectionRoutes = SETTINGS_SECTIONS.map((section) =>
  createRoute({
    getParentRoute: () => settingsRoute,
    path: section.path,
    component: section.Component,
  }),
);

export const routeTree = rootRoute.addChildren([
  homeRoute,
  allChatsRoute,
  chatRoute,
  settingsRoute.addChildren([settingsIndexRoute, ...settingsSectionRoutes]),
]);

/**
 * Hash history, because the renderer is loaded from a custom protocol with no
 * server behind it to rewrite unknown paths onto the document. A path history
 * would work until the first reload of a deep link.
 */
export const router = createRouter({
  routeTree,
  history: createHashHistory(),
  defaultPreload: false,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
