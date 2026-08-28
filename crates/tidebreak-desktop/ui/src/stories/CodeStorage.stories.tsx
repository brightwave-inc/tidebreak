import { useEffect, useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { CodeStorageSnapshot, HarnessKind } from "@/api/types";
import { CodeStoragePage } from "@/code/CodeStoragePage";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { SidebarExpandStrip } from "@/sidebar/SidebarExpandStrip";
import { useUiStore } from "@/UiStore";
import {
  deliveryCodeRepo,
  deliveryWorkspaces,
  harnessDoctor,
} from "./fixtures";

const POPULATED: CodeStorageSnapshot = {
  repos: [
    {
      id: deliveryCodeRepo.id,
      display_name: deliveryCodeRepo.display_name,
      clone_bytes: 1_200_000_000,
      clone_reclaimable: false,
      workspaces: [
        {
          id: deliveryWorkspaces[0]?.id ?? "ws-1",
          title: deliveryWorkspaces[0]?.title ?? "Active workspace",
          status: "active",
          on_disk_bytes: 850_000_000,
          next_action: "archive",
          next_reclaim_bytes: 850_000_000,
        },
        {
          id: "ws-archived",
          title: "Archived experiment",
          status: "archived",
          on_disk_bytes: 12_288,
          next_action: "release",
          next_reclaim_bytes: 12_288,
        },
        {
          id: "ws-released",
          title: "Released cleanup",
          status: "released",
          on_disk_bytes: 4096,
          next_reclaim_bytes: 0,
        },
      ],
    },
  ],
};

function idleSocket(): WebSocket {
  const socket = {
    onopen: null as WebSocket["onopen"],
    onclose: null as WebSocket["onclose"],
    onerror: null as WebSocket["onerror"],
    close() {},
    addEventListener() {},
    removeEventListener() {},
  } as unknown as WebSocket;
  queueMicrotask(() => socket.onopen?.(new Event("open")));
  return socket;
}

function storyClient(report: CodeStorageSnapshot): ApiClient {
  return {
    openCodeUpdates: () => idleSocket(),
    listCodeStorage: async () => report,
    listCodeRepos: async () => [deliveryCodeRepo],
    listCodeWorkspaces: async () => deliveryWorkspaces,
    getHarnessDoctor: async () => harnessDoctor,
    listCodeHarnessModels: async (kind: HarnessKind) => ({ kind, models: [] }),
    getCodeCloneDefaults: async () => ({
      gh_found: true,
      gh_authenticated: true,
      gh_remediation: "",
    }),
    startCodeWatch: async () => ({}) as never,
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
  const storageRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/storage",
    component: CodeStoragePage,
  });
  const placeholders = [
    "/",
    "/code",
    "/code/delivery/pull-requests",
    "/code/archive",
    "/code/analytics",
    "/settings",
  ].map((path) =>
    createRoute({
      getParentRoute: () => rootRoute,
      path,
      component: () => <p className="p-6">{path}</p>,
    }),
  );
  const workspaceRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/code/w/$workspaceId",
    component: () => <p className="p-6">Workspace</p>,
  });
  return createRouter({
    routeTree: rootRoute.addChildren([
      ...placeholders,
      workspaceRoute,
      storageRoute,
    ]),
    history: createMemoryHistory({ initialEntries: ["/code/storage"] }),
  });
}

function resetStoryState() {
  disconnectCodeUpdates();
  useCodeCatalogStore.getState().reset();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({
    railPrefs: DEFAULT_RAIL_PREFS,
    reviewSidebarOpen: false,
    newWorkspaceOpen: false,
    addRepoOpen: false,
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  useUiStore.setState({ sidebarCollapsed: false, sidebarWidth: 280 });
}

function CodeStorageStory({ report }: { report: CodeStorageSnapshot }) {
  const [state] = useState(() => {
    resetStoryState();
    useCodeCatalogStore.setState({
      repos: [deliveryCodeRepo],
      workspaces: deliveryWorkspaces,
      loaded: true,
      error: null,
    });
    return { client: storyClient(report), router: storyRouter() };
  });
  useEffect(
    () => () => {
      disconnectCodeUpdates();
      useCodeCatalogStore.getState().reset();
      useCodeUpdatesStore.getState().reset();
    },
    [],
  );
  return (
    <AppContextProvider value={appContext(state.client)}>
      <div className="app-shell h-full min-h-0 w-full overflow-hidden">
        <SidebarExpandStrip macOverlay />
        <div className="app-body">
          <RouterProvider router={state.router as never} />
        </div>
      </div>
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Storage",
  component: CodeStorageStory,
  args: { report: POPULATED },
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof CodeStorageStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Populated: Story = {};

export const Empty: Story = {
  args: { report: { repos: [] } },
};
