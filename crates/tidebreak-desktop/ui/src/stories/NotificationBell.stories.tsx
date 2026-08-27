import type { Meta, StoryObj } from "@storybook/react-vite";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import type { AgentNotification } from "@/api";
import { AppContextProvider } from "@/AppContext";
import { NotificationBellButton } from "@/NotificationBellButton";
import { useNotifications } from "@/NotificationStore";
import { storyAppContext, storyClient } from "./routeStoryHarness";

const unread: AgentNotification = {
  id: "n-unread",
  kind: "agent_completed",
  title: "Review the updater migration finished",
  context: { surface: "chat", chatId: "chat-1" },
  createdAt: "2026-08-26T18:00:00.000Z",
  readAt: null,
};

const read: AgentNotification = {
  id: "n-read",
  kind: "agent_failed",
  title:
    "A very long workspace title that should truncate in the dense row failed",
  context: { surface: "code", sessionId: "s-1", workspaceId: "ws-1" },
  createdAt: "2026-08-26T17:00:00.000Z",
  readAt: "2026-08-26T17:05:00.000Z",
};

const invalidTimestamp: AgentNotification = {
  ...unread,
  id: "n-invalid-time",
  createdAt: "not-a-timestamp",
};

function BellSurface({
  notifications,
  unreadCount,
  loaded = true,
}: {
  notifications: AgentNotification[];
  unreadCount: number;
  loaded?: boolean;
}) {
  useNotifications.setState({
    notifications,
    unread: unreadCount,
    loaded,
  });
  const rootRoute = createRootRoute({
    component: () => (
      <AppContextProvider value={storyAppContext(storyClient())}>
        <div className="w-64 border-r border-border-subtle bg-page-background p-2">
          <NotificationBellButton defaultOpen />
        </div>
      </AppContextProvider>
    ),
  });
  const indexRoute = createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => null,
  });
  const router = createRouter({
    routeTree: rootRoute.addChildren([indexRoute]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return <RouterProvider router={router} />;
}

const meta = {
  title: "Shell/Notification bell",
  component: BellSurface,
} satisfies Meta<typeof BellSurface>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Empty: Story = {
  args: { notifications: [], unreadCount: 0 },
};

export const Unread: Story = {
  args: { notifications: [unread, read], unreadCount: 1 },
};

export const Read: Story = {
  args: { notifications: [read], unreadCount: 0 },
};

export const LongTitle: Story = {
  args: { notifications: [read], unreadCount: 0 },
};

export const InvalidTimestamp: Story = {
  args: { notifications: [invalidTimestamp], unreadCount: 1 },
};
