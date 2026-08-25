import { useEffect } from "react";
import {
  createHashHistory,
  createRootRoute,
  createRoute,
  createRouter,
  useNavigate,
} from "@tanstack/react-router";

import { AppShell } from "./AppShell";
import { AppsPage } from "./apps/AppsPage";
import { ChatRoute } from "./ChatRoute";
import { HomeRoute } from "./HomeRoute";
import { InboxView } from "./InboxView";
import { useManagedPolicy } from "./managedPolicy";
import { panelSearchFrom, type PanelSearch } from "./panel/panelUrl";
import { PluginsPage } from "./plugins/PluginsPage";
import { ProjectFilesView } from "./ProjectFilesView";
import { useProjectListStore } from "./ProjectListStore";
import { RouteFrame } from "./RouteFrame";
import { SettingsRoute } from "./SettingsRoute";
import { defaultSettingsPathFor, SETTINGS_SECTIONS } from "./settings/sections";
import { AppSidebar } from "./sidebar/AppSidebar";
import { CodeHome } from "./code/CodeHome";
import { CodeAnalyticsPage } from "./code/CodeAnalyticsPage";
import { CodeArchivePage } from "./code/CodeArchivePage";
import {
  CodeDeliveryPage,
  codeDeliverySearchFrom,
} from "./code/CodeDeliveryPage";
import { CodeNotificationsPage } from "./code/CodeNotificationsPage";
import { CodeWorkspacePage } from "./code/CodeWorkspacePage";

const rootRoute = createRootRoute({ component: AppShell });

/**
 * A conversation's URL carries its layout and, when arrived at from the inbox,
 * the parked call the transcript should reveal.
 */
type ChatSearch = PanelSearch & { focus?: string; at?: string };

const homeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  // Home hosts panels the way a conversation does — the Apps library opens
  // beside the composer — so it reads the same layout params. Which panel
  // types home actually accepts is decided by HomeRoute, not the URL parser.
  validateSearch: (search: Record<string, unknown>): PanelSearch =>
    panelSearchFrom(search),
  component: HomeRoute,
});

/**
 * The install-wide libraries, each a full page with the shared rail. They used
 * to open as tabs beside a conversation; nothing about an app or a plugin is
 * scoped to one, so they take the pane the way the inbox does.
 */
const appsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/apps",
  component: () => <AppsPage />,
});

const appDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/apps/$appId",
  component: AppDetailRouteComponent,
});

function AppDetailRouteComponent() {
  const { appId } = appDetailRoute.useParams();
  return <AppsPage appId={appId} />;
}

const pluginsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/plugins",
  component: () => <PluginsPage />,
});

const pluginDetailRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/plugins/$pluginId",
  component: PluginDetailRouteComponent,
});

function PluginDetailRouteComponent() {
  const { pluginId } = pluginDetailRoute.useParams();
  return <PluginsPage pluginId={pluginId} />;
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
    <RouteFrame sidebar={<AppSidebar />}>
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
    ...panelSearchFrom(search),
    // Where a deep link is pointing: the parked call to reveal once the
    // transcript is up. It rides beside the layout params because it is
    // addressing state like they are, and it is dropped from the URL as soon
    // as it has been honored so a reload does not re-scroll.
    focus: typeof search.focus === "string" ? search.focus : undefined,
    // Hash history already owns the fragment, so transcript anchoring travels
    // as query state. Unlike `focus`, it remains until the reader returns to
    // the live tail, which makes an anchored reload land in the same place.
    at: typeof search.at === "string" ? search.at : undefined,
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

/**
 * The same conversation, addressed through the project holding it.
 *
 * A chat inside a project is not a different kind of conversation, so this
 * renders the identical route with the identical layout params — the path only
 * says where the reader came from, which is what lets the rail keep the
 * project's row open and highlighted. A chat that has since moved is not
 * redirected: the id in the path is the conversation, and reopening the wrong
 * project's link still lands on the right transcript.
 */
const projectChatRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/p/$projectId/c/$chatId",
  validateSearch: chatRoute.options.validateSearch,
  component: ProjectChatRouteComponent,
});

function ProjectChatRouteComponent() {
  const { projectId, chatId } = projectChatRoute.useParams();
  // Opening a conversation through its project opens the project. Without this
  // a deep link lands on a transcript whose row is inside a collapsed folder,
  // so the rail shows the reader nothing of where they are.
  useEffect(() => {
    useProjectListStore.getState().expandProject(projectId);
  }, [projectId]);
  return <ChatRoute key={chatId} chatId={chatId} />;
}

/**
 * A project's own page: the files its conversations share.
 *
 * Reached from the project's row rather than by opening it, because opening a
 * project means opening the conversations inside it — this page is about the
 * material behind them, which a reader visits far less often.
 */
const projectRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/p/$projectId",
  component: ProjectRouteComponent,
});

function ProjectRouteComponent() {
  const { projectId } = projectRoute.useParams();
  useEffect(() => {
    useProjectListStore.getState().expandProject(projectId);
  }, [projectId]);
  return (
    <RouteFrame sidebar={<AppSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-auto">
        <ProjectFilesView projectId={projectId} />
      </div>
    </RouteFrame>
  );
}

const codeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code",
  component: CodeHome,
});

const codeWorkspaceRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code/w/$workspaceId",
  validateSearch: (search: Record<string, unknown>): PanelSearch =>
    panelSearchFrom(search),
  component: CodeWorkspaceRouteComponent,
});

const codeAnalyticsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code/analytics",
  component: CodeAnalyticsPage,
});

const codeDeliveryPullRequestsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code/delivery/pull-requests",
  validateSearch: codeDeliverySearchFrom,
  component: CodeDeliveryPullRequestsRoute,
});

const codeDeliveryRunsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code/delivery/runs",
  validateSearch: codeDeliverySearchFrom,
  component: CodeDeliveryRunsRoute,
});

function CodeDeliveryPullRequestsRoute() {
  return (
    <CodeDeliveryPage
      surface="pull_requests"
      search={codeDeliveryPullRequestsRoute.useSearch()}
    />
  );
}

function CodeDeliveryRunsRoute() {
  return (
    <CodeDeliveryPage
      surface="runs"
      search={codeDeliveryRunsRoute.useSearch()}
    />
  );
}

const codeArchiveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code/archive",
  component: CodeArchivePage,
});

const codeNotificationsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/code/notifications",
  component: CodeNotificationsPage,
});

function CodeWorkspaceRouteComponent() {
  const { workspaceId } = codeWorkspaceRoute.useParams();
  return <CodeWorkspacePage workspaceId={workspaceId} />;
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
  return <p className="text-muted-foreground p-6 text-sm">Opening settings…</p>;
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
  return <p className="text-muted-foreground p-6 text-sm">Opening settings…</p>;
}

const settingsMcpRedirectRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "mcp",
  component: McpSettingsRedirect,
});

/**
 * Code mode graduated from the Experimental panel. Preserve old bookmarks by
 * landing on the settings page that now owns its harness setup.
 */
function ExperimentalSettingsRedirect() {
  const navigate = useNavigate();
  useEffect(() => {
    const codingHarnessesPath: string = "/settings/coding-harnesses";
    void navigate({ to: codingHarnessesPath, replace: true });
  }, [navigate]);
  return <p className="text-muted-foreground p-6 text-sm">Opening settings…</p>;
}

const settingsExperimentalRedirectRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: "experimental",
  component: ExperimentalSettingsRedirect,
});

const settingsSectionRoutes = SETTINGS_SECTIONS.map((section) =>
  createRoute({
    getParentRoute: () => settingsRoute,
    path: section.path,
    component: section.Component,
    // Only the sections that address with search params declare one; the rest
    // have nothing to validate.
    ...(section.validateSearch
      ? { validateSearch: section.validateSearch }
      : {}),
  }),
);

export const routeTree = rootRoute.addChildren([
  homeRoute,
  appsRoute,
  appDetailRoute,
  pluginsRoute,
  pluginDetailRoute,
  inboxRoute,
  chatRoute,
  projectRoute,
  projectChatRoute,
  codeRoute,
  codeWorkspaceRoute,
  codeAnalyticsRoute,
  codeDeliveryPullRequestsRoute,
  codeDeliveryRunsRoute,
  codeArchiveRoute,
  codeNotificationsRoute,
  settingsRoute.addChildren([
    settingsIndexRoute,
    settingsMcpRedirectRoute,
    settingsExperimentalRedirectRoute,
    ...settingsSectionRoutes,
  ]),
]);

/**
 * Hash history, because the renderer is loaded from a custom protocol with no
 * server behind it to rewrite unknown paths onto the document. A path history
 * would work until the first reload of a deep link.
 */
export function createAppRouter() {
  return createRouter({
    routeTree,
    history: createHashHistory(),
    defaultPreload: false,
  });
}

export type AppRouter = ReturnType<typeof createAppRouter>;

declare module "@tanstack/react-router" {
  interface Register {
    router: AppRouter;
  }
}
