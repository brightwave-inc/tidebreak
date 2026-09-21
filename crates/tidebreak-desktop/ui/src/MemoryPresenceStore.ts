import { create } from "zustand";

import type { ApiClient, MemoryDigest, MemorySettings } from "./api";

/** What the store reads from the connected client. */
export type MemoryPresenceClient = Pick<
  ApiClient,
  "getSettings" | "getMemoryDigest"
>;

/** The one snapshot every memory surface reads from. */
export type MemoryFacts = {
  settings: MemorySettings;
  digest: MemoryDigest;
};

type MemoryPresenceStore = {
  facts: MemoryFacts | null;
  /** The read in flight, so concurrent callers share it instead of racing. */
  inFlight: Promise<MemoryFacts> | null;
  /**
   * Read settings and the personal digest once, and hand the same promise to
   * anyone who asks while it is still running.
   */
  refresh: (client: MemoryPresenceClient) => Promise<MemoryFacts>;
  /** Fold a write's response in, so every reader sees the change at once. */
  apply: (facts: Partial<MemoryFacts>) => void;
  reset: () => void;
};

/**
 * Memory settings and the personal digest, held once for the whole app.
 *
 * The settings page reads and writes them, and the activity chip in every
 * conversation summarizes them. One store means the two surfaces cannot
 * issue competing one-off reads, and a change made on the settings page is
 * what the chip shows the moment the user returns to a conversation.
 */
export const useMemoryPresenceStore = create<MemoryPresenceStore>()(
  (set, get) => ({
    facts: null,
    inFlight: null,
    refresh(client) {
      const running = get().inFlight;
      if (running) return running;
      const read = Promise.all([
        client.getSettings(),
        client.getMemoryDigest({ kind: "personal" }),
      ])
        .then(([runtime, digest]) => {
          const facts = { settings: runtime.memory, digest };
          set({ facts, inFlight: null });
          return facts;
        })
        .catch((caught: unknown) => {
          // The last snapshot stays; the caller decides what a failed read
          // means for its own surface.
          set({ inFlight: null });
          throw caught;
        });
      set({ inFlight: read });
      return read;
    },
    apply(next) {
      const current = get().facts;
      if (current == null) return;
      set({ facts: { ...current, ...next } });
    },
    reset() {
      set({ facts: null, inFlight: null });
    },
  }),
);
