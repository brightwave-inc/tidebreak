import { useEffect, useRef } from "react";
import { useRouterState } from "@tanstack/react-router";
import { toast } from "sonner";

import type { AgentNotification, ApiClient } from "./api";
import {
  isWindowFocused,
  presentNativeNotification,
  requestUserAttention,
} from "./host";
import {
  notificationPresent,
  viewingNotificationConversation,
} from "./notificationPresent";
import { desktopNotificationsEnabled } from "./NotificationPreferences";
import { useNotifications } from "./NotificationStore";
import { useRefreshSignals } from "./RefreshSignals";

const POLL_INTERVAL_MS = 10_000;

const storeActions = useNotifications.getState();

export function notificationHref(row: AgentNotification): string {
  if (row.context.surface === "chat") {
    return `/c/${row.context.chatId}`;
  }
  return `/code/w/${row.context.workspaceId}`;
}

/**
 * Polls the durable agent-finished log and presents each new unread row once.
 */
export function useAgentNotifications(client: ApiClient | null): void {
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const pathnameRef = useRef(pathname);
  pathnameRef.current = pathname;
  const presentedRef = useRef<Set<string>>(new Set());
  const refreshRef = useRef<(() => void) | null>(null);
  const notificationsSignal = useRefreshSignals((state) => state.notifications);
  const notifications = useNotifications((state) => state.notifications);

  useEffect(() => {
    if (!client) {
      storeActions.clear();
      presentedRef.current = new Set();
      refreshRef.current = null;
      return;
    }
    storeActions.clear();
    presentedRef.current = new Set();
    let cancelled = false;
    let seq = 0;
    let initialized = false;

    const read = async () => {
      const thisSeq = ++seq;
      try {
        const [page, unread] = await Promise.all([
          client.listNotifications(),
          client.notificationUnreadCount(),
        ]);
        if (cancelled || thisSeq !== seq) return;
        storeActions.setPage(page.notifications, unread);
        if (!initialized) {
          initialized = true;
          presentedRef.current = new Set(
            page.notifications.map((row) => row.id),
          );
          return;
        }
        const fresh = page.notifications.filter(
          (row) => !row.readAt && !presentedRef.current.has(row.id),
        );
        for (const row of fresh) {
          presentedRef.current.add(row.id);
          void presentRow(row, pathnameRef.current, client).catch(() => {
            void requestUserAttention().catch(() => {});
          });
        }
      } catch (error) {
        if (!cancelled && thisSeq === seq) {
          console.error("failed to refresh notifications", error);
        }
      }
    };

    refreshRef.current = () => {
      void read();
    };
    void read();
    const interval = window.setInterval(() => void read(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      seq += 1;
      window.clearInterval(interval);
      refreshRef.current = null;
    };
  }, [client]);

  const lastSignalRef = useRef(notificationsSignal);
  useEffect(() => {
    if (lastSignalRef.current === notificationsSignal) return;
    lastSignalRef.current = notificationsSignal;
    refreshRef.current?.();
  }, [notificationsSignal]);

  useEffect(() => {
    if (!client) return;
    const open = notifications.filter(
      (row) =>
        !row.readAt && viewingNotificationConversation(pathname, row.context),
    );
    if (open.length === 0) return;
    const ids = open.map((row) => row.id);
    void client
      .markNotificationsRead(ids)
      .then(() => {
        useNotifications.getState().markRead(ids);
      })
      .catch(() => {});
  }, [client, notifications, pathname]);
}

async function presentRow(
  row: AgentNotification,
  pathname: string,
  client: ApiClient,
): Promise<void> {
  const windowFocused = await isWindowFocused();
  const kind = notificationPresent({
    windowFocused,
    viewingConversation: viewingNotificationConversation(pathname, row.context),
    permission: windowFocused ? "granted" : "prompt",
    enabled: desktopNotificationsEnabled(),
  });
  if (kind === "skip") return;
  if (kind === "toast") {
    toast(row.title, {
      action: {
        label: "Open",
        onClick: () => {
          window.location.hash = `#${notificationHref(row)}`;
          void client.markNotificationsRead([row.id]);
        },
      },
    });
    return;
  }
  if (kind === "native") {
    const shown = await presentNativeNotification(row.title, "");
    if (!shown) {
      void requestUserAttention().catch(() => {});
    }
    return;
  }
  void requestUserAttention().catch(() => {});
}
