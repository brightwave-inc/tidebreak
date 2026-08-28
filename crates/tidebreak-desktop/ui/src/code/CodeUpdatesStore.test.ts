import { afterEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type {
  Attention,
  CodeCloneJobSnapshot,
  CodeSessionDigest,
  CodeUpdateNotice,
} from "../api/types";
import {
  activateCodeCloneClient,
  codeClientGeneration,
  connectCodeUpdates,
  disconnectCodeUpdates,
  noticeToAction,
  reconcileCodeClone,
  reduceCodeUpdates,
  selectCodeClone,
  shouldRequestOsAttention,
  takeSelectedCodeClone,
  trackCodeClone,
  useCodeUpdatesStore,
  watchChildren,
  workspaceDigest,
  type CodeUpdatesState,
} from "./CodeUpdatesStore";
import { useCodeUiStore } from "./CodeUiStore";

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

const working: Attention = { state: { type: "working" }, source: "lifecycle" };
const need: Attention = {
  state: {
    type: "needs_you",
    prompt: "an approval is waiting",
    source: "structured",
  },
  source: "structured",
};

function digest(overrides: Partial<CodeSessionDigest> = {}): CodeSessionDigest {
  return {
    workspace: "ws-1",
    session: "sess-1",
    kind: "interactive",
    lifecycle: "idle",
    attention: working,
    title: "first change",
    turn_count: 0,
    ...overrides,
  };
}

const EMPTY_STATE: CodeUpdatesState = {
  conversationsByWorkspace: {},
  childrenByWorkspace: {},
  cloneJobs: {},
  cloneTracking: {},
  cloneReadErrors: {},
  cloneClientGeneration: null,
  selectedClone: null,
  harnessInstalls: {},
  viewedWorkspaceId: null,
  deliveryRevision: 0,
  turnRewrites: {},
};

afterEach(() => {
  disconnectCodeUpdates();
  useCodeUpdatesStore.getState().reset();
  useCodeUiStore.setState({ addRepoOpen: false });
  vi.useRealTimers();
  vi.clearAllMocks();
  vi.restoreAllMocks();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

class FakeSocket {
  onopen: WebSocket["onopen"] = null;
  onclose: WebSocket["onclose"] = null;
  onerror: WebSocket["onerror"] = null;
  closed = false;

  close() {
    this.closed = true;
  }
}

function pendingClone(id: string): CodeCloneJobSnapshot {
  return { id, phase: "receiving objects", percent: 40, done: false };
}

function completeClone(id: string): CodeCloneJobSnapshot {
  return {
    id,
    phase: "complete",
    done: true,
    repo_id: `repo-${id}`,
  };
}

function cloneClient(getCodeCloneJob: ApiClient["getCodeCloneJob"]): {
  client: Pick<ApiClient, "getCodeCloneJob" | "openCodeUpdates">;
  sockets: FakeSocket[];
  send: (notice: CodeUpdateNotice, index?: number) => void;
} {
  const sockets: FakeSocket[] = [];
  const listeners: Array<(notice: CodeUpdateNotice) => void> = [];
  const client = {
    getCodeCloneJob: vi.fn(getCodeCloneJob),
    openCodeUpdates: vi.fn((listener: (notice: CodeUpdateNotice) => void) => {
      const socket = new FakeSocket();
      sockets.push(socket);
      listeners.push(listener);
      return socket as unknown as WebSocket;
    }),
  };
  return {
    client,
    sockets,
    send: (notice, index = listeners.length - 1) => listeners[index]?.(notice),
  };
}

describe("reduceCodeUpdates", () => {
  it("replaces the map on snapshot and upserts a digest", () => {
    const afterSnapshot = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [
        digest(),
        digest({ workspace: "ws-2", session: "sess-2", title: "other" }),
      ],
    });
    expect(Object.keys(afterSnapshot.conversationsByWorkspace)).toEqual([
      "ws-1",
      "ws-2",
    ]);
    const afterDigest = reduceCodeUpdates(afterSnapshot, {
      type: "digest",
      digest: digest({ turn_count: 2, attention: need }),
    });
    const first = afterDigest.conversationsByWorkspace["ws-1"]["sess-1"];
    expect(first.turn_count).toBe(2);
    expect(first.attention).toEqual(need);
    expect(afterDigest.conversationsByWorkspace["ws-2"]["sess-2"].title).toBe(
      "other",
    );
    const restated = reduceCodeUpdates(afterDigest, {
      type: "snapshot",
      sessions: [digest({ workspace: "ws-3", session: "sess-3" })],
    });
    expect(Object.keys(restated.conversationsByWorkspace)).toEqual(["ws-3"]);
  });

  it("keeps every agent in one workspace and collapses them for a card", () => {
    const seeded = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [
        digest({ session: "sess-1" }),
        digest({ session: "sess-2", lifecycle: "running" }),
      ],
    });
    // Record 54: several agents share one worktree, so the map keeps them all.
    expect(Object.keys(seeded.conversationsByWorkspace["ws-1"])).toEqual([
      "sess-1",
      "sess-2",
    ]);
    // A running agent outranks an idle one on the card.
    expect(workspaceDigest(seeded, "ws-1")?.session).toBe("sess-2");

    const asked = reduceCodeUpdates(seeded, {
      type: "digest",
      digest: digest({ session: "sess-1", attention: need }),
    });
    // A waiting agent outranks a running one: it is the one that needs a person.
    expect(workspaceDigest(asked, "ws-1")?.session).toBe("sess-1");
    expect(workspaceDigest(asked, "ws-missing")).toBeUndefined();
  });

  it("keeps an ended conversation so its title and PR survive", () => {
    const seeded = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [digest()],
    });
    const ended = reduceCodeUpdates(seeded, {
      type: "digest",
      digest: digest({ lifecycle: "ended" }),
    });
    expect(ended.conversationsByWorkspace["ws-1"]["sess-1"].lifecycle).toBe(
      "ended",
    );
  });

  it("keeps watch digests beside the conversation, never in its slot", () => {
    const seeded = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [
        digest(),
        digest({ session: "sess-watch", kind: "watch", lifecycle: "running" }),
      ],
    });
    // ADR 0050: a watch is a child, never one of the conversations.
    expect(Object.keys(seeded.conversationsByWorkspace["ws-1"])).toEqual([
      "sess-1",
    ]);
    expect(watchChildren(seeded, "ws-1").map((child) => child.session)).toEqual(
      ["sess-watch"],
    );

    const afterWatchDigest = reduceCodeUpdates(seeded, {
      type: "digest",
      digest: digest({
        session: "sess-watch",
        kind: "watch",
        lifecycle: "running",
        turn_count: 5,
      }),
    });
    expect(
      Object.keys(afterWatchDigest.conversationsByWorkspace["ws-1"]),
    ).toEqual(["sess-1"]);
    expect(
      afterWatchDigest.conversationsByWorkspace["ws-1"]["sess-1"].turn_count,
    ).toBe(0);
    expect(watchChildren(afterWatchDigest, "ws-1")[0]?.turn_count).toBe(5);
  });

  it("drops an ended watch child and rebuilds children on snapshot", () => {
    const seeded = reduceCodeUpdates(EMPTY_STATE, {
      type: "snapshot",
      sessions: [
        digest(),
        digest({ session: "sess-watch", kind: "watch", lifecycle: "running" }),
      ],
    });
    const ended = reduceCodeUpdates(seeded, {
      type: "digest",
      digest: digest({
        session: "sess-watch",
        kind: "watch",
        lifecycle: "ended",
      }),
    });
    expect(watchChildren(ended, "ws-1")).toEqual([]);

    // A reconnect snapshot that no longer lists the watch heals a missed end.
    const healed = reduceCodeUpdates(seeded, {
      type: "snapshot",
      sessions: [digest()],
    });
    expect(watchChildren(healed, "ws-1")).toEqual([]);
  });

  it("maps notices onto reducer actions", () => {
    expect(
      noticeToAction({
        type: "snapshot",
        sessions: [digest()],
      }),
    ).toEqual({ type: "snapshot", sessions: [digest()] });
    expect(
      noticeToAction({
        type: "digest",
        workspace: "ws-1",
        session: "sess-1",
        kind: "interactive",
        harness_kind: "claude_code",
        lifecycle: "running",
        attention: working,
        title: "first change",
        turn_count: 1,
        activity: "monitor",
      }),
    ).toEqual({
      type: "digest",
      digest: {
        workspace: "ws-1",
        session: "sess-1",
        kind: "interactive",
        harness_kind: "claude_code",
        lifecycle: "running",
        attention: working,
        title: "first change",
        turn_count: 1,
        activity: "monitor",
      },
    });
    expect(
      noticeToAction({
        type: "terminal_activity",
        workspace_id: "ws-1",
        terminal_id: "term-1",
      }),
    ).toBeNull();
    expect(
      noticeToAction({
        type: "clone_progress",
        job: "job-1",
        phase: "receiving objects",
        percent: 40,
        done: false,
      }),
    ).toEqual({
      type: "clone_progress",
      job: {
        id: "job-1",
        phase: "receiving objects",
        percent: 40,
        done: false,
      },
    });
    expect(
      noticeToAction({
        type: "harness_install",
        kind: "claude_code",
        version: "2.1.234",
        phase: "installing",
        done: false,
      }),
    ).toEqual({
      type: "harness_install",
      install: {
        kind: "claude_code",
        version: "2.1.234",
        phase: "installing",
        done: false,
      },
    });
    expect(noticeToAction({ type: "delivery" })).toEqual({ type: "delivery" });
  });

  it("bumps the delivery revision on each nudge (decision 66)", () => {
    const first = reduceCodeUpdates(EMPTY_STATE, { type: "delivery" });
    expect(first.deliveryRevision).toBe(1);
    const second = reduceCodeUpdates(first, { type: "delivery" });
    expect(second.deliveryRevision).toBe(2);
  });

  it("keeps one install state per engine", () => {
    const installing = reduceCodeUpdates(EMPTY_STATE, {
      type: "harness_install",
      install: { kind: "codex", phase: "installing", done: false },
    });
    expect(installing.harnessInstalls.codex?.phase).toBe("installing");
    const ready = reduceCodeUpdates(installing, {
      type: "harness_install",
      install: { kind: "codex", phase: "ready", done: true },
    });
    expect(ready.harnessInstalls.codex).toEqual({
      kind: "codex",
      phase: "ready",
      done: true,
    });
  });

  it("does not let late clone progress replace a terminal result", () => {
    const completed = reduceCodeUpdates(EMPTY_STATE, {
      type: "clone_progress",
      job: completeClone("job-1"),
    });
    const stale = reduceCodeUpdates(completed, {
      type: "clone_progress",
      job: pendingClone("job-1"),
    });
    expect(stale.cloneJobs["job-1"]).toEqual(completeClone("job-1"));
  });
});

describe("clone onboarding reconciliation", () => {
  it("notifies once when terminal socket progress beats the start response", () => {
    const { client } = cloneClient(async (jobId) => completeClone(jobId));
    activateCodeCloneClient(client);
    useCodeUpdatesStore.getState().apply({
      type: "clone_progress",
      job: completeClone("job-fast"),
    });

    trackCodeClone(
      client,
      pendingClone("job-fast"),
      { github: "acme/fast" },
      true,
    );

    expect(useCodeUpdatesStore.getState().cloneJobs["job-fast"]).toEqual(
      completeClone("job-fast"),
    );
    expect(
      useCodeUpdatesStore.getState().cloneTracking["job-fast"]?.notified,
    ).toBe(true);
    expect(toast.success).toHaveBeenCalledTimes(1);
  });

  it("binds each background notification action to its clone", () => {
    const { client } = cloneClient(async (jobId) => pendingClone(jobId));
    activateCodeCloneClient(client);
    trackCodeClone(client, pendingClone("job-a"), { github: "acme/a" }, true);
    trackCodeClone(client, pendingClone("job-b"), { github: "acme/b" }, true);

    useCodeUpdatesStore.getState().apply({
      type: "clone_progress",
      job: completeClone("job-a"),
    });
    const options = vi.mocked(toast.success).mock.calls[0]?.[1] as
      | { action?: { onClick: () => void } }
      | undefined;
    options?.action?.onClick();

    expect(useCodeUiStore.getState().addRepoOpen).toBe(true);
    expect(takeSelectedCodeClone(client)?.job.id).toBe("job-a");
    expect(useCodeUpdatesStore.getState().selectedClone).toBeNull();
  });

  it("ignores a notification from a replaced client generation", () => {
    const first = cloneClient(async (jobId) => pendingClone(jobId)).client;
    activateCodeCloneClient(first);
    trackCodeClone(
      first,
      pendingClone("job-shared"),
      { github: "acme/old" },
      true,
    );
    useCodeUpdatesStore.getState().apply({
      type: "clone_progress",
      job: completeClone("job-shared"),
    });
    const options = vi.mocked(toast.success).mock.calls[0]?.[1] as
      | { action?: { onClick: () => void } }
      | undefined;

    const replacement = cloneClient(async (jobId) =>
      pendingClone(jobId),
    ).client;
    activateCodeCloneClient(replacement);
    trackCodeClone(
      replacement,
      pendingClone("job-shared"),
      { github: "acme/new" },
      true,
    );
    options?.action?.onClick();

    expect(useCodeUiStore.getState().addRepoOpen).toBe(false);
    expect(useCodeUpdatesStore.getState().selectedClone).toBeNull();
  });

  it("does not fall back when a selected clone belongs to the old client", () => {
    const first = cloneClient(async (jobId) => pendingClone(jobId)).client;
    const firstGeneration = activateCodeCloneClient(first);
    trackCodeClone(first, pendingClone("job-old"), { github: "acme/old" });
    expect(
      selectCodeClone({
        jobId: "job-old",
        clientGeneration: firstGeneration,
      }),
    ).toBe(true);

    const replacement = cloneClient(async (jobId) =>
      pendingClone(jobId),
    ).client;
    activateCodeCloneClient(replacement);
    trackCodeClone(replacement, pendingClone("job-new"), {
      github: "acme/new",
    });

    expect(takeSelectedCodeClone(replacement)).toBeNull();
    expect(useCodeUpdatesStore.getState().selectedClone).toBeNull();
  });

  it("keeps tracked clone state when live Code updates disconnect", () => {
    const { client } = cloneClient(async (jobId) => pendingClone(jobId));
    activateCodeCloneClient(client);
    trackCodeClone(
      client,
      pendingClone("job-persisted"),
      { url: "https://example.com/acme/app.git" },
      true,
    );

    connectCodeUpdates(client);
    disconnectCodeUpdates();

    expect(useCodeUpdatesStore.getState().cloneJobs["job-persisted"]).toEqual(
      pendingClone("job-persisted"),
    );
    expect(
      useCodeUpdatesStore.getState().cloneTracking["job-persisted"]?.background,
    ).toBe(true);
  });

  it("reconciles every active clone when the updates socket opens", async () => {
    const { client, sockets } = cloneClient(async (jobId) =>
      pendingClone(jobId),
    );
    activateCodeCloneClient(client);
    trackCodeClone(client, pendingClone("job-a"), { github: "acme/a" }, true);
    trackCodeClone(client, pendingClone("job-b"), { github: "acme/b" }, true);

    connectCodeUpdates(client);
    sockets[0]?.onopen?.call(
      sockets[0] as unknown as WebSocket,
      new Event("open"),
    );

    await vi.waitFor(() =>
      expect(client.getCodeCloneJob).toHaveBeenCalledTimes(2),
    );
    expect(client.getCodeCloneJob).toHaveBeenCalledWith("job-a");
    expect(client.getCodeCloneJob).toHaveBeenCalledWith("job-b");
  });

  it("recovers a background completion after Code updates reconnect", async () => {
    const { client, sockets } = cloneClient(async (jobId) =>
      completeClone(jobId),
    );
    activateCodeCloneClient(client);
    trackCodeClone(
      client,
      pendingClone("job-reconnect"),
      { github: "acme/app" },
      true,
    );

    connectCodeUpdates(client);
    disconnectCodeUpdates();
    connectCodeUpdates(client);
    sockets[1]?.onopen?.call(
      sockets[1] as unknown as WebSocket,
      new Event("open"),
    );

    await vi.waitFor(() =>
      expect(
        useCodeUpdatesStore.getState().cloneJobs["job-reconnect"]?.done,
      ).toBe(true),
    );
    expect(toast.success).toHaveBeenCalledWith(
      "Repository cloned",
      expect.objectContaining({
        description: "Create a workspace when you are ready.",
      }),
    );
  });

  it("shows one terminal notice when socket and durable results overlap", async () => {
    const durable = deferred<CodeCloneJobSnapshot>();
    const { client, sockets, send } = cloneClient(() => durable.promise);
    activateCodeCloneClient(client);
    trackCodeClone(
      client,
      pendingClone("job-overlap"),
      { url: "https://example.com/acme/app.git" },
      true,
    );
    connectCodeUpdates(client);
    sockets[0]?.onopen?.call(
      sockets[0] as unknown as WebSocket,
      new Event("open"),
    );
    await vi.waitFor(() =>
      expect(client.getCodeCloneJob).toHaveBeenCalledWith("job-overlap"),
    );

    send({
      type: "clone_progress",
      job: "job-overlap",
      phase: "complete",
      done: true,
      repo_id: "repo-job-overlap",
    });
    durable.resolve(pendingClone("job-overlap"));
    await durable.promise;
    await Promise.resolve();

    expect(toast.success).toHaveBeenCalledTimes(1);
    expect(useCodeUpdatesStore.getState().cloneJobs["job-overlap"]).toEqual(
      completeClone("job-overlap"),
    );
  });

  it("clears tracked clone state for a replacement ApiClient", () => {
    const first = cloneClient(async (jobId) => pendingClone(jobId)).client;
    const replacement = cloneClient(async (jobId) =>
      pendingClone(jobId),
    ).client;
    activateCodeCloneClient(first);
    trackCodeClone(first, pendingClone("job-old"), { github: "acme/old" });

    const replacementGeneration = activateCodeCloneClient(replacement);

    expect(useCodeUpdatesStore.getState().cloneJobs).toEqual({});
    expect(useCodeUpdatesStore.getState().cloneTracking).toEqual({});
    expect(useCodeUpdatesStore.getState().cloneClientGeneration).toBe(
      replacementGeneration,
    );
    expect(replacementGeneration).not.toBe(codeClientGeneration(first));
  });

  it("ignores a durable result from an old ApiClient generation", async () => {
    const staleRead = deferred<CodeCloneJobSnapshot>();
    const first = cloneClient(() => staleRead.promise).client;
    const replacement = cloneClient(async (jobId) =>
      pendingClone(jobId),
    ).client;
    activateCodeCloneClient(first);
    trackCodeClone(
      first,
      pendingClone("job-shared"),
      { github: "acme/old" },
      true,
    );
    const reconciliation = reconcileCodeClone(first, "job-shared");

    activateCodeCloneClient(replacement);
    trackCodeClone(
      replacement,
      pendingClone("job-shared"),
      { github: "acme/new" },
      true,
    );
    staleRead.resolve(completeClone("job-shared"));
    await expect(reconciliation).resolves.toBeNull();

    expect(useCodeUpdatesStore.getState().cloneJobs["job-shared"]).toEqual(
      pendingClone("job-shared"),
    );
    expect(toast.success).not.toHaveBeenCalled();
  });
});

describe("shouldRequestOsAttention", () => {
  it("fires only on a transition into structured NeedsYou for a workspace that is not being viewed", () => {
    expect(shouldRequestOsAttention(working, need, "ws-1", null)).toBe(true);
    expect(shouldRequestOsAttention(undefined, need, "ws-1", null)).toBe(true);
    expect(shouldRequestOsAttention(need, need, "ws-1", null)).toBe(false);
    expect(shouldRequestOsAttention(working, need, "ws-1", "ws-1")).toBe(false);
    expect(shouldRequestOsAttention(working, need, "ws-1", "ws-other")).toBe(
      true,
    );
    expect(
      shouldRequestOsAttention(
        working,
        { state: { type: "done_unreviewed" }, source: "lifecycle" },
        "ws-1",
        null,
      ),
    ).toBe(false);
  });
});

describe("turn rewrite notices", () => {
  it("keeps stored rewrite text when a later rewriting notice omits it", () => {
    const rewritten = reduceCodeUpdates(EMPTY_STATE, {
      type: "turn_rewrite",
      session: "sess-1",
      turnId: "t1",
      state: "rewritten",
      rewrite: "The turn added three tools.",
    });
    const lagged = reduceCodeUpdates(rewritten, {
      type: "turn_rewrite",
      session: "sess-1",
      turnId: "t1",
      state: "rewriting",
    });
    expect(lagged.turnRewrites["sess-1"]?.["t1"]).toEqual({
      state: "rewriting",
      rewrite: "The turn added three tools.",
    });
  });
});
