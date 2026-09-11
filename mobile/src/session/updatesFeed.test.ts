import { beforeEach, describe, expect, it } from "vitest";
import type {
  MachineClient,
  ReconnectingSocketHandlers,
  connectWithBackoff,
} from "../lib/machine";
import { acquireUpdatesFeed, disposeSharedFeed } from "./updatesFeed";
import { useUpdatesStore } from "./updatesStore";

type FakeSocket = {
  handlers: ReconnectingSocketHandlers;
  started: number;
  disposed: number;
};

function fakeConnect(): {
  sockets: FakeSocket[];
  connect: typeof connectWithBackoff;
} {
  const sockets: FakeSocket[] = [];
  const connect: typeof connectWithBackoff = (_open, handlers) => {
    const record: FakeSocket = { handlers, started: 0, disposed: 0 };
    sockets.push(record);
    return {
      start: () => {
        record.started += 1;
      },
      refresh: () => {},
      dispose: () => {
        record.disposed += 1;
      },
    };
  };
  return { sockets, connect };
}

const clientA = { name: "a" } as unknown as MachineClient;
const clientB = { name: "b" } as unknown as MachineClient;

const snapshotNotice = JSON.stringify({
  type: "snapshot",
  sessions: [
    {
      workspace: "ws-1",
      session: "sess-1",
      kind: "interactive",
      lifecycle: "running",
      attention: { state: { type: "working" }, source: "lifecycle" },
      title: "first change",
      turn_count: 1,
    },
  ],
});

describe("shared updates feed", () => {
  beforeEach(() => {
    disposeSharedFeed();
  });

  it("shares one socket across consumers and feeds the store", () => {
    const { sockets, connect } = fakeConnect();
    const first = acquireUpdatesFeed(clientA, () => {}, connect);
    const second = acquireUpdatesFeed(clientA, () => {}, connect);
    expect(sockets).toHaveLength(1);
    expect(sockets[0]!.started).toBe(1);

    sockets[0]!.handlers.onMessage(snapshotNotice);
    expect(useUpdatesStore.getState().snapshotReceived).toBe(true);
    expect(useUpdatesStore.getState().order).toEqual(["sess-1"]);

    first.release();
    expect(sockets[0]!.disposed).toBe(0);
    expect(useUpdatesStore.getState().snapshotReceived).toBe(true);

    second.release();
    second.release();
    expect(sockets[0]!.disposed).toBe(1);
    expect(useUpdatesStore.getState().snapshotReceived).toBe(false);
  });

  it("notifies consumers of connection state, including late joiners", () => {
    const { sockets, connect } = fakeConnect();
    const seen: string[] = [];
    const first = acquireUpdatesFeed(clientA, (state) => seen.push(state), connect);
    expect(first.state).toBe("reconnecting");
    sockets[0]!.handlers.onConnectionState?.("live");
    expect(seen).toEqual(["live"]);

    const second = acquireUpdatesFeed(clientA, () => {}, connect);
    expect(second.state).toBe("live");
    first.release();
    second.release();
  });

  it("replaces the feed and resets the store when the machine changes", () => {
    const { sockets, connect } = fakeConnect();
    const first = acquireUpdatesFeed(clientA, () => {}, connect);
    sockets[0]!.handlers.onMessage(snapshotNotice);
    expect(useUpdatesStore.getState().snapshotReceived).toBe(true);

    const second = acquireUpdatesFeed(clientB, () => {}, connect);
    expect(sockets).toHaveLength(2);
    expect(sockets[0]!.disposed).toBe(1);
    expect(useUpdatesStore.getState().snapshotReceived).toBe(false);

    // The stale consumer's release must not tear down the new feed.
    first.release();
    expect(sockets[1]!.disposed).toBe(0);
    second.release();
    expect(sockets[1]!.disposed).toBe(1);
  });
});
