import { afterEach, describe, expect, it, vi } from "vitest";

import type { MemoryDigest, MemorySettings } from "./api";
import { memorySummary } from "./chatMemoryPresence";
import { useMemoryPresenceStore } from "./MemoryPresenceStore";

const digest = (record_count: number): MemoryDigest => ({
  scope: { kind: "personal" },
  markdown: "",
  byte_len: 0,
  byte_cap: 8192,
  record_count,
});

const on: MemorySettings = {
  enabled: true,
  capture_enabled: true,
  capture_ready: true,
};

afterEach(() => useMemoryPresenceStore.getState().reset());

describe("memorySummary", () => {
  it("phrases every state the row can be in", () => {
    const off = { ...on, enabled: false };
    expect(memorySummary({ settings: off, digest: digest(4) }, false)).toBe(
      "Off",
    );
    expect(memorySummary({ settings: on, digest: digest(4) }, true)).toBe(
      "Off for this conversation",
    );
    expect(memorySummary({ settings: on, digest: digest(0) }, false)).toBe(
      "On · nothing approved yet",
    );
    expect(memorySummary({ settings: on, digest: digest(1) }, false)).toBe(
      "1 record in context",
    );
    expect(memorySummary({ settings: on, digest: digest(3) }, false)).toBe(
      "3 records in context",
    );
  });
});

describe("useMemoryPresenceStore", () => {
  it("shares one read between concurrent callers and folds writes in", async () => {
    const getSettings = vi.fn(async () => ({ memory: on }) as never);
    const getMemoryDigest = vi.fn(async () => digest(2));
    const client = { getSettings, getMemoryDigest };
    const store = useMemoryPresenceStore.getState();
    // The chip and the settings page asking at once is one round trip.
    const [a, b] = await Promise.all([
      store.refresh(client),
      store.refresh(client),
    ]);
    expect(a).toBe(b);
    expect(getSettings).toHaveBeenCalledOnce();
    expect(getMemoryDigest).toHaveBeenCalledOnce();
    // A settings write lands in the snapshot without another read.
    store.apply({ settings: { ...on, enabled: false } });
    expect(useMemoryPresenceStore.getState().facts?.settings.enabled).toBe(
      false,
    );
    expect(getSettings).toHaveBeenCalledOnce();
  });

  it("keeps the last snapshot when a refresh fails, and reports the failure", async () => {
    const store = useMemoryPresenceStore.getState();
    await store.refresh({
      getSettings: async () => ({ memory: on }) as never,
      getMemoryDigest: async () => digest(1),
    });
    await expect(
      store.refresh({
        getSettings: async () => {
          throw new Error("offline");
        },
        getMemoryDigest: async () => digest(1),
      }),
    ).rejects.toThrow("offline");
    expect(useMemoryPresenceStore.getState().facts?.digest.record_count).toBe(
      1,
    );
  });
});
