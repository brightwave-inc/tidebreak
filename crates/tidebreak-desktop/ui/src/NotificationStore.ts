import { create } from "zustand";

import type { AgentNotification } from "./api";

export type NotificationStore = {
  notifications: AgentNotification[];
  unread: number;
  loaded: boolean;
  setPage: (notifications: AgentNotification[], unread: number) => void;
  markRead: (ids: string[]) => void;
  markAllRead: () => void;
  clear: () => void;
};

function samePage(
  left: AgentNotification[],
  right: AgentNotification[],
): boolean {
  return (
    left.length === right.length &&
    left.every((row, index) => {
      const other = right[index];
      return (
        !!other &&
        row.id === other.id &&
        row.readAt === other.readAt &&
        row.title === other.title
      );
    })
  );
}

export const useNotifications = create<NotificationStore>()((set) => ({
  notifications: [],
  unread: 0,
  loaded: false,
  setPage: (notifications, unread) =>
    set((state) =>
      state.loaded &&
      state.unread === unread &&
      samePage(state.notifications, notifications)
        ? state
        : { notifications, unread, loaded: true },
    ),
  markRead: (ids) =>
    set((state) => {
      const marked = new Set(ids);
      const readAt = new Date().toISOString();
      let changed = 0;
      const notifications = state.notifications.map((row) => {
        if (row.readAt || !marked.has(row.id)) return row;
        changed += 1;
        return { ...row, readAt };
      });
      return changed === 0
        ? state
        : {
            notifications,
            unread: Math.max(0, state.unread - changed),
          };
    }),
  markAllRead: () =>
    set((state) => {
      if (state.unread === 0) return state;
      const readAt = new Date().toISOString();
      return {
        notifications: state.notifications.map((row) =>
          row.readAt ? row : { ...row, readAt },
        ),
        unread: 0,
      };
    }),
  clear: () => set({ notifications: [], unread: 0, loaded: false }),
}));
