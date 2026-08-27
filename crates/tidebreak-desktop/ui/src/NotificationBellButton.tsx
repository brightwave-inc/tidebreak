import { useNavigate } from "@tanstack/react-router";
import { formatDistanceToNow } from "date-fns";
import { Bell, CircleAlert, CircleCheck } from "lucide-react";

import type { AgentNotification } from "./api";
import { useApp } from "./AppContext";
import {
  Empty,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "./components/ui/empty";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "./components/ui/popover";
import { cn } from "./lib/utils";
import { notificationHref } from "./useAgentNotifications";
import { useNotifications } from "./NotificationStore";
import { SidebarButton } from "./sidebar/primitives";

export function NotificationBellButton({
  defaultOpen = false,
}: {
  defaultOpen?: boolean;
}) {
  const navigate = useNavigate();
  const { client } = useApp();
  const notifications = useNotifications((state) => state.notifications);
  const unread = useNotifications((state) => state.unread);
  const loaded = useNotifications((state) => state.loaded);

  const reload = async () => {
    const [page, count] = await Promise.all([
      client.listNotifications().catch(() => null),
      client.notificationUnreadCount().catch(() => null),
    ]);
    if (page && count !== null) {
      useNotifications.getState().setPage(page.notifications, count);
    }
  };

  const open = (row: AgentNotification) => {
    void navigate({ to: notificationHref(row) });
    if (row.readAt) return;
    useNotifications.getState().markRead([row.id]);
    void client.markNotificationsRead([row.id]).catch(() => reload());
  };

  const markAll = async () => {
    useNotifications.getState().markAllRead();
    const marked = await client.markAllNotificationsRead().catch(() => null);
    if (marked === null) await reload();
  };

  return (
    <Popover defaultOpen={defaultOpen}>
      <PopoverTrigger asChild>
        <SidebarButton
          type="button"
          aria-label={
            unread > 0 ? `Notifications, ${unread} unread` : "Notifications"
          }
        >
          <span className="relative inline-flex">
            <Bell />
            {unread > 0 && (
              <span
                className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-foreground"
                aria-hidden="true"
              />
            )}
          </span>
          <span>Notifications</span>
        </SidebarButton>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        side="right"
        className="w-80 p-0"
        aria-label="Notifications"
      >
        <div className="flex items-center justify-between gap-2 border-b border-border-subtle px-3 py-2">
          <p className="text-sm font-medium">Notifications</p>
          {unread > 0 && (
            <button
              type="button"
              className="text-xs text-muted-foreground underline-offset-2 hover:underline"
              onClick={() => void markAll()}
            >
              Mark all read
            </button>
          )}
        </div>
        {notifications.length === 0 ? (
          <Empty className="py-8">
            <EmptyHeader>
              <EmptyMedia variant="icon">
                <Bell aria-hidden="true" />
              </EmptyMedia>
              <EmptyTitle>
                {loaded ? "No notifications" : "Loading…"}
              </EmptyTitle>
            </EmptyHeader>
          </Empty>
        ) : (
          <ul className="max-h-80 overflow-y-auto py-1">
            {notifications.map((row) => (
              <li key={row.id}>
                <button
                  type="button"
                  className={cn(
                    "flex w-full items-start gap-2 px-3 py-2 text-left text-sm hover:bg-accent",
                    !row.readAt && "font-medium",
                  )}
                  onClick={() => open(row)}
                >
                  {row.kind === "agent_failed" ? (
                    <CircleAlert
                      className="mt-0.5 size-3.5 shrink-0 text-critical"
                      aria-hidden="true"
                    />
                  ) : (
                    <CircleCheck
                      className="mt-0.5 size-3.5 shrink-0 text-success"
                      aria-hidden="true"
                    />
                  )}
                  <span className="min-w-0 flex-1">
                    <span className="flex min-w-0 items-center gap-2">
                      <span className="min-w-0 flex-1 truncate">
                        {row.title}
                      </span>
                      {!row.readAt && (
                        <span
                          className="size-1.5 shrink-0 rounded-full bg-foreground"
                          aria-label="Unread"
                        />
                      )}
                    </span>
                    <span className="block text-2xs font-normal text-muted-foreground">
                      {notificationRelativeTime(row.createdAt)}
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </PopoverContent>
    </Popover>
  );
}

export function notificationRelativeTime(value: string): string {
  const createdAt = new Date(value);
  if (!Number.isFinite(createdAt.getTime())) return "Unknown time";
  try {
    return formatDistanceToNow(createdAt, { addSuffix: true });
  } catch {
    return "Unknown time";
  }
}
