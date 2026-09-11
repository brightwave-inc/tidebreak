import type { MachineClient, ReconnectingSocket } from "../lib/machine";
import { connectWithBackoff } from "../lib/machine";
import { isCodeUpdateNotice, noticeToAction } from "../lib/updates";
import { useUpdatesStore } from "./updatesStore";

export type FeedConnectionState = "live" | "reconnecting";

export type UpdatesFeedHandle = {
  state: FeedConnectionState;
  refresh: () => void;
  release: () => void;
};

type SharedFeed = {
  client: MachineClient;
  conn: ReconnectingSocket;
  refs: number;
  state: FeedConnectionState;
  listeners: Set<(state: FeedConnectionState) => void>;
};

let feed: SharedFeed | null = null;

/**
 * Acquire the machine-wide /updates connection. Screens stack on top of each
 * other while all reading the same digest store, so the socket is shared and
 * refcounted: the store resets only when the machine changes or the last
 * consumer releases, never because one screen in the stack unmounted.
 */
export function acquireUpdatesFeed(
  client: MachineClient,
  onState: (state: FeedConnectionState) => void,
  connect: typeof connectWithBackoff = connectWithBackoff,
): UpdatesFeedHandle {
  if (feed && feed.client !== client) {
    disposeSharedFeed();
  }
  if (!feed) {
    const created: SharedFeed = {
      client,
      conn: connect(() => client.openSocket("/updates"), {
        onMessage: (data) => {
          try {
            const parsed: unknown = JSON.parse(data);
            if (!isCodeUpdateNotice(parsed)) return;
            const action = noticeToAction(parsed);
            if (action) useUpdatesStore.getState().apply(action);
          } catch {
            // Drop malformed notices; the next snapshot heals the list.
          }
        },
        onConnectionState: (state) => {
          created.state = state;
          for (const listener of created.listeners) listener(state);
        },
      }),
      refs: 0,
      state: "reconnecting",
      listeners: new Set(),
    };
    feed = created;
    created.conn.start();
  }
  const active = feed;
  active.refs += 1;
  active.listeners.add(onState);
  let released = false;
  return {
    state: active.state,
    refresh: () => active.conn.refresh(),
    release: () => {
      if (released) return;
      released = true;
      active.listeners.delete(onState);
      active.refs -= 1;
      if (active.refs === 0 && feed === active) {
        disposeSharedFeed();
      }
    },
  };
}

export function disposeSharedFeed(): void {
  if (!feed) return;
  feed.conn.dispose();
  feed = null;
  useUpdatesStore.getState().reset();
}
