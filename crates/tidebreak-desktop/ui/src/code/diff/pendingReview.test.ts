import { describe, expect, it } from "vitest";

import {
  commentsReadyToSend,
  createPendingReviewStore,
  readStoredReview,
  reviewSummary,
} from "./pendingReview";
import type { ReviewComment } from "./reviewComments";

/** Local storage as a reload sees it: whatever the last page wrote. */
function memoryStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => void values.delete(key),
    setItem: (key, value) => void values.set(key, value),
  };
}

function comment(id: string, path = "src/queue.ts"): ReviewComment {
  return {
    id,
    author: { kind: "person" },
    path,
    lines: [{ kind: "add", oldNo: null, newNo: 3, text: "const MAX = 20;" }],
    body: `Comment ${id}`,
    createdAt: "2026-09-24T10:00:00.000Z",
  };
}

describe("pending review", () => {
  it("survives a reload", () => {
    const storage = memoryStorage();
    const before = createPendingReviewStore(storage);
    before.getState().add("ws-1", comment("c1"));
    before.getState().add("ws-1", comment("c2", "src/lib.rs"));
    before.getState().edit("ws-1", "c2", "Edited");

    const after = createPendingReviewStore(storage);
    expect(after.getState().byWorkspace["ws-1"]).toEqual([
      comment("c1"),
      { ...comment("c2", "src/lib.rs"), body: "Edited" },
    ]);
  });

  it("forgets what was sent, and never stores the in-flight marks", () => {
    const storage = memoryStorage();
    const store = createPendingReviewStore(storage);
    store.getState().add("ws-1", comment("c1"));
    const claimed = store.getState().claim("ws-1");
    store.getState().add("ws-1", comment("c2"));

    expect(claimed.map((item) => item.id)).toEqual(["c1"]);
    expect(commentsReadyToSend("ws-1", store).map((item) => item.id)).toEqual([
      "c2",
    ]);
    expect(
      reviewSummary(
        store.getState().byWorkspace["ws-1"]!,
        new Set(store.getState().sending["ws-1"]),
      ),
    ).toEqual({ count: 1, files: 1, sending: 1 });

    store.getState().finishSend("ws-1", claimed, true);
    expect(
      createPendingReviewStore(storage)
        .getState()
        .byWorkspace["ws-1"]?.map((item) => item.id),
    ).toEqual(["c2"]);
  });

  it("gives each comment to one send, however many start at once", () => {
    const store = createPendingReviewStore(memoryStorage());
    store.getState().add("ws-1", comment("c1"));
    const first = store.getState().claim("ws-1");
    const second = store.getState().claim("ws-1");
    expect(first.map((item) => item.id)).toEqual(["c1"]);
    expect(second).toEqual([]);
  });

  it("keeps a comment edited while its send was out, for the next message", () => {
    const store = createPendingReviewStore(memoryStorage());
    store.getState().add("ws-1", comment("c1"));
    store.getState().add("ws-1", comment("c2"));
    const claimed = store.getState().claim("ws-1");
    store.getState().edit("ws-1", "c2", "Said better");
    store.getState().finishSend("ws-1", claimed, true);
    expect(store.getState().byWorkspace["ws-1"]).toEqual([
      { ...comment("c2"), body: "Said better" },
    ]);
    expect(commentsReadyToSend("ws-1", store)).toHaveLength(1);
  });

  it("puts refused comments back, and clears only the ones not sending", () => {
    const store = createPendingReviewStore(memoryStorage());
    store.getState().add("ws-1", comment("c1"));
    const claimed = store.getState().claim("ws-1");
    store.getState().add("ws-1", comment("c2"));
    store.getState().clear("ws-1");
    expect(
      store.getState().byWorkspace["ws-1"]?.map((item) => item.id),
    ).toEqual(["c1"]);
    store.getState().finishSend("ws-1", claimed, false);
    expect(commentsReadyToSend("ws-1", store).map((item) => item.id)).toEqual([
      "c1",
    ]);
  });

  it("records where a comment's lines went, and writes nothing when they stayed", () => {
    const store = createPendingReviewStore(memoryStorage());
    store.getState().add("ws-1", comment("c1"));
    const before = store.getState().byWorkspace;
    store.getState().relocate("ws-1", "c1", {
      lines: comment("c1").lines,
      outdated: false,
    });
    expect(store.getState().byWorkspace).toBe(before);

    store.getState().relocate("ws-1", "c1", {
      lines: [{ kind: "add", oldNo: null, newNo: 4, text: "const MAX = 20;" }],
      outdated: false,
    });
    expect(store.getState().byWorkspace["ws-1"]?.[0]?.lines[0]?.newNo).toBe(4);

    store.getState().relocate("ws-1", "c1", { outdated: true });
    expect(store.getState().byWorkspace["ws-1"]?.[0]).toMatchObject({
      outdated: true,
      lines: [{ newNo: 4, text: "const MAX = 20;" }],
    });
  });

  it("takes comments back without doubling one it already holds", () => {
    const store = createPendingReviewStore(memoryStorage());
    store.getState().add("ws-1", comment("c1"));
    store.getState().restore("ws-1", [comment("c1"), comment("c2")]);
    expect(
      store.getState().byWorkspace["ws-1"]?.map((item) => item.id),
    ).toEqual(["c1", "c2"]);
  });

  it("drops stored comments that no longer read as comments", () => {
    const storage = memoryStorage();
    storage.setItem(
      "tidebreak.code-pending-review.v1",
      JSON.stringify({
        version: 1,
        workspaces: {
          "ws-1": [comment("c1"), { id: "broken", lines: [] }],
          "ws-2": "not a list",
        },
      }),
    );
    expect(readStoredReview(storage)).toEqual({ "ws-1": [comment("c1")] });
    storage.setItem("tidebreak.code-pending-review.v1", "{not json");
    expect(readStoredReview(storage)).toEqual({});
  });
});
