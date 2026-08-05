import type { ReactNode } from "react";
import { render } from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import type { PanelSearch } from "../panel/panelUrl";

/**
 * Renders a component inside a router shaped like the real one, so anything
 * that reads the URL — panel layout, chat id, navigation — behaves as it does
 * in the app. Returns the router, whose location is what a navigation
 * assertion should be made against.
 */
export async function renderWithRouter(
  ui: ReactNode,
  { initialUrl = "/c/chat-1" }: { initialUrl?: string } = {},
) {
  const rootRoute = createRootRoute();
  const chatRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/c/$chatId",
    validateSearch: (
      search: Record<string, unknown>,
    ): PanelSearch & { focus?: string } => ({
      left: typeof search.left === "string" ? search.left : undefined,
      right: typeof search.right === "string" ? search.right : undefined,
      fullscreen: typeof search.fullscreen === "string" ? search.fullscreen : undefined,
      focus: typeof search.focus === "string" ? search.focus : undefined,
    }),
    component: () => <>{ui}</>,
  });
  const homeRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => <>{ui}</>,
  });
  const settingsRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: () => <>{ui}</>,
  });
  const appDetailRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/apps/$appId",
    component: () => <>{ui}</>,
  });
  const webSearchSettingsRoute = createRoute({
    getParentRoute: () => settingsRoute,
    path: "web-search",
    component: () => <>{ui}</>,
  });
  const voiceSettingsRoute = createRoute({
    getParentRoute: () => settingsRoute,
    path: "voice-transcription",
    component: () => <>{ui}</>,
  });

  const router = createRouter({
    routeTree: rootRoute.addChildren([
      homeRoute,
      chatRoute,
      appDetailRoute,
      settingsRoute.addChildren([webSearchSettingsRoute, voiceSettingsRoute]),
    ]),
    history: createMemoryHistory({ initialEntries: [initialUrl] }),
  });

  // The first match resolves asynchronously, and mounting before it lands
  // renders an empty document that every query then fails against.
  await router.load();

  // The router's own generics are registered against the app's route tree; a
  // throwaway tree for one test does not need to satisfy them.
  const result = render(<RouterProvider router={router as never} />);
  return { ...result, router };
}
