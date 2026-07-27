import {
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from "@tanstack/react-router";

import { AppShell } from "./AppShell";
import { ChatRoute } from "./ChatRoute";
import { HomeRoute } from "./HomeRoute";
import type { PanelSearch } from "./panel/panelUrl";
import { SettingsRoute } from "./SettingsRoute";

const rootRoute = createRootRoute({ component: AppShell });

const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: HomeRoute,
});

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

export const routeTree = rootRoute.addChildren([homeRoute, chatRoute, settingsRoute]);

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
