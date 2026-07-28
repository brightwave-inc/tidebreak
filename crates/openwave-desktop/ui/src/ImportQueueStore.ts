import { create } from "zustand";
import {
  listenForLibraryImportProgress,
  type LibraryImportProgress,
  type LibraryImportState,
} from "./documents";
import { hasNativeHost } from "./host";

export type ImportQueueEntry = LibraryImportProgress & {
  updatedAt: number;
};

export type ImportQueueStore = {
  entries: ImportQueueEntry[];
  receive: (progress: LibraryImportProgress) => void;
  dismissCleanRun: () => void;
};

export function createImportQueueStore() {
  return create<ImportQueueStore>()((set) => ({
    entries: [],
    receive: (progress) =>
      set((state) => {
        const next: ImportQueueEntry = { ...progress, updatedAt: Date.now() };
        const index = state.entries.findIndex((entry) => entry.importId === progress.importId);
        if (index === -1) return { entries: [...state.entries, next] };
        const entries = [...state.entries];
        entries[index] = next;
        return { entries };
      }),
    // A failed run stays visible until its reader has seen and resolved it.
    dismissCleanRun: () =>
      set((state) =>
        state.entries.some((entry) => entry.status === "failed") ||
        state.entries.some((entry) => importIsActive(entry.status))
          ? state
          : { entries: [] },
      ),
  }));
}

export const useImportQueueStore = createImportQueueStore();

export function importIsActive(status: LibraryImportState): boolean {
  return status === "queued" || status === "streaming";
}

export function sortedImportQueue(entries: ImportQueueEntry[]): ImportQueueEntry[] {
  return [...entries].sort((left, right) => {
    const leftFailed = left.status === "failed";
    const rightFailed = right.status === "failed";
    if (leftFailed !== rightFailed) return leftFailed ? -1 : 1;
    return left.updatedAt - right.updatedAt;
  });
}

let listening = false;

/** Start once at application boot so imports remain visible across navigation. */
export function startImportQueue(): void {
  if (listening || !hasNativeHost()) return;
  listening = true;
  void listenForLibraryImportProgress((progress) => {
    useImportQueueStore.getState().receive(progress);
  }).catch(() => {
    // The native bridge may not be ready during startup. Import commands still
    // surface their own errors, and a later app restart installs the listener.
    listening = false;
  });
}
