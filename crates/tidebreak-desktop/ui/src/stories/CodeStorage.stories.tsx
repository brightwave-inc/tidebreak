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
import type { CodeStorageSnapshot } from "@/api/types";
import { CodeStoragePage } from "@/code/CodeStoragePage";
import { useCodeCatalogStore } from "@/code/CodeCatalogStore";
import { DEFAULT_RAIL_PREFS, useCodeUiStore } from "@/code/CodeUiStore";
import {
  disconnectCodeUpdates,
  useCodeUpdatesStore,
} from "@/code/CodeUpdatesStore";
import { SidebarExpandStrip } from "@/sidebar/SidebarExpandStrip";
import { useUiStore } from "@/UiStore";
import { deliveryCodeRepo, deliveryWorkspaces } from "./fixtures";

const REPORT: CodeStorageSnapshot = {
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

function app(report: CodeStorageSnapshot): AppContextValue {
  return {
    client: {
      listCodeStorage: async () => report,
      listCodeRepos: async () => [deliveryCodeRepo],
      listCodeWorkspaces: async () => deliveryWorkspaces,
      getHarnessDoctor: async () => ({ harnesses: [] }),
      getCodeCloneDefaults: async () => ({
        parent_dir: "/tmp/src",
        gh_found: false,
        gh_remediation: "gh is not installed.",
      }),
      openCodeUpdates: () => idleSocket(),
    } as unknown as ApiClient,
    attachment: "local",
    serverInfo: null,
    setServerInfo: () => {},
    connectedFolderPaths: [],
    refreshConnectedFolderPaths: async () => {},
  };
}

function StorageHarness({ report }: { report: CodeStorageSnapshot }) {
  useCodeCatalogStore.setState({
    repos: [deliveryCodeRepo],
    workspaces: deliveryWorkspaces,
    loaded: true,
    error: null,
  });
  useCodeUiStore.setState({ railPrefs: DEFAULT_RAIL_PREFS });
  useUiStore.setState({ sidebarCollapsed: false });
  useCodeUpdatesStore.setState({ connected: true });
  const root = createRootRoute();
  const storage = createRoute({
    getParentRoute: () => root,
    path: "/code/storage",
    component: CodeStoragePage,
  });
  const router = createRouter({
    history: createMemoryHistory({ initialEntries: ["/code/storage"] }),
    routeTree: root.addChildren([storage]),
  });
  return (
    <AppContextProvider value={app(report)}>
      <SidebarExpandStrip />
      <RouterProvider router={router} />
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Storage",
  component: StorageHarness,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => {
      disconnectCodeUpdates();
      return <Story />;
    },
  ],
} satisfies Meta<typeof StorageHarness>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Populated: Story = {
  args: { report: REPORT },
};

export const Empty: Story = {
  args: { report: { repos: [] } },
};
