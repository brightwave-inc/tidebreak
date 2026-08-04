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
import { InboxView } from "./InboxView";
import { useManagedPolicy } from "./managedPolicy";
import type { PanelSearch } from "./panel/panelUrl";
import { RouteFrame } from "./RouteFrame";
import { SettingsRoute } from "./SettingsRoute";
import { defaultSettingsPathFor, SETTINGS_SECTIONS } from "./settings/sections";
import { HomeSidebar } from "./sidebar/HomeSidebar";

const rootRoute = createRootRoute({ component: AppShell });

/**
 * A conversation's URL carries its layout and, when arrived at from the inbox,
 * the parked call the transcript should reveal.
 */
type ChatSearch = PanelSearch & { focus?: string };

const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  // Home hosts panels the way a conversation does — the Apps library opens
  // beside the composer — so it reads the same layout params. Which panel
  // types home actually accepts is decided by HomeRoute, not the URL parser.
  validateSearch: (search: Record<string, unknown>): PanelSearch => ({
    left: typeof search.left === "string" ? search.left : undefined,
    right: typeof search.right === "string" ? search.right : undefined,
    fullscreen: typeof search.fullscreen === "string" ? search.fullscreen : undefined,
  }),
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

/**
 * The inbox shares home's rail: what is waiting spans conversations, so it is
 * not scoped to one either.
 */
const inboxRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/inbox",
  component: InboxRoute,
});

function InboxRoute() {
  return (
    <RouteFrame sidebar={<HomeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-hidden">
        <InboxView />
      </div>
    </RouteFrame>
  );
}

const chatRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/c/$chatId",
  validateSearch: (search: Record<string, unknown>): ChatSearch => ({
    left: typeof search.left === "string" ? search.left : undefined,
    right: typeof search.right === "string" ? search.right : undefined,
    fullscreen: typeof search.fullscreen === "string" ? search.fullscreen : undefined,
    // Where a deep link is pointing: the parked call to reveal once the
    // transcript is up. It rides beside the layout params because it is
    // addressing state like they are, and it is dropped from the URL as soon
    // as it has been honored so a reload does not re-scroll.
    focus: typeof search.focus === "string" ? search.focus : undefined,
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

/**
 * The standalone MCP servers page was absorbed into Connected apps; stale
 * deep links and history entries still name its old path, so it lands there
 * rather than on a blank route.
 */
function McpSettingsRedirect() {
  const navigate = useNavigate();
  useEffect(() => {
    // Settings sections are registered from a runtime table, so TanStack's
    // generated route union contains `/settings` but not each literal child.
    const connectedAppsPath: string = "/settings/connected-apps";
    void navigate({ to: connectedAppsPath, replace: true });
  }, [navigate]);
  return (
    <p className="text-muted-foreground p-6 text-sm">Opening settings…</p>
  );
}

const settingsMcpRedirectRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "mcp",
  component: McpSettingsRedirect,
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
  inboxRoute,
  chatRoute,
  settingsRoute.addChildren([
    settingsIndexRoute,
    settingsMcpRedirectRoute,
    ...settingsSectionRoutes,
  ]),
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
