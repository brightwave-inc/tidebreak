import { describe, expect, it } from "vitest";
import {
  createImportQueueStore,
  sortedImportQueue,
  type ImportQueueEntry,
} from "./ImportQueueStore";

const queued = {
  importId: "11111111-1111-4111-8111-111111111111",
  displayName: "notes.md",
  status: "queued" as const,
  documentId: null,
  processingStatus: null,
  message: null,
};

describe("ImportQueueStore", () => {
  it("keeps imports outside a component and advances one item by its native id", () => {
    const store = createImportQueueStore();
    store.getState().receive(queued);
    store.getState().receive({
      ...queued,
      status: "imported",
      documentId: "22222222-2222-4222-8222-222222222222",
      processingStatus: "queued",
    });

    expect(store.getState().entries).toEqual([
      expect.objectContaining({
        importId: queued.importId,
        status: "imported",
        documentId: "22222222-2222-4222-8222-222222222222",
      }),
    ]);
  });

  it("automatically dismisses only a completed run with no failures", () => {
    const store = createImportQueueStore();
    store.getState().receive({ ...queued, status: "streaming" });
    store.getState().dismissCleanRun();
    expect(store.getState().entries).toHaveLength(1);

    store.getState().receive({
      ...queued,
      status: "failed",
      message: "The file is empty",
    });
    store.getState().dismissCleanRun();
    expect(store.getState().entries).toHaveLength(1);

    const clean = createImportQueueStore();
    clean.getState().receive({
      ...queued,
      status: "imported",
      documentId: "22222222-2222-4222-8222-222222222222",
      processingStatus: "queued",
    });
    clean.getState().dismissCleanRun();
    expect(clean.getState().entries).toEqual([]);
  });

  it("lets the reader manually dismiss a finished run even after a failure", () => {
    const store = createImportQueueStore();
    store.getState().receive({ ...queued, status: "streaming" });
    // Nothing to dismiss while the run is still working.
    store.getState().dismiss();
    expect(store.getState().entries).toHaveLength(1);

    store.getState().receive({
      ...queued,
      status: "failed",
      message: "The file is empty",
    });
    store.getState().dismiss();
    expect(store.getState().entries).toEqual([]);
  });

  it("puts failures before active and completed imports", () => {
    const entries: ImportQueueEntry[] = [
      { ...queued, status: "imported", updatedAt: 1 },
      {
        ...queued,
        importId: "33333333-3333-4333-8333-333333333333",
        status: "failed",
        message: "Unreadable",
        updatedAt: 3,
      },
      {
        ...queued,
        importId: "44444444-4444-4444-8444-444444444444",
        status: "streaming",
        updatedAt: 2,
      },
    ];
    expect(sortedImportQueue(entries).map((entry) => entry.status)).toEqual([
      "failed",
      "imported",
      "streaming",
    ]);
  });

  it("keeps an expanded archive's successes and failures independent", () => {
    const store = createImportQueueStore();
    store.getState().receive(queued);
    store.getState().receive({
      ...queued,
      status: "imported",
      documentId: "22222222-2222-4222-8222-222222222222",
      processingStatus: "queued",
    });
    store.getState().receive({
      ...queued,
      importId: "55555555-5555-4555-8555-555555555555",
      displayName: "oversized.pdf",
      status: "failed",
      message: "Archive entries are limited to 128 MiB",
    });

    expect(store.getState().entries).toHaveLength(2);
    expect(sortedImportQueue(store.getState().entries).map((entry) => entry.status)).toEqual([
      "failed",
      "imported",
    ]);
  });
});
