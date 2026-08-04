import { create } from "zustand";

/**
 * The one native file-or-folder picker the host will have open at a time.
 *
 * Every surface that opens one — a folder-access decision, connecting a folder,
 * confirming a previously approved one, importing a source, exporting an output
 * — ends up at the same host mutex, and the host rejects a second call outright
 * rather than queueing it. So the latch that keeps a reader from starting a
 * second one has to be app-wide and shared by all of them: a latch held by only
 * one caller does not prevent the collision, it just decides which caller gets
 * the raw host error.
 *
 * Keyed by holder so the surface that owns the open picker can still show its
 * own progress while every other one reads as unavailable.
 */
export type NativePickerLatchStore = {
  holder: string | null;
  /** Take the picker for this holder, or report that another one has it. */
  claim: (holder: string) => boolean;
  release: (holder: string) => void;
};

/** Stable holders for the surfaces that are not keyed by a call id. */
export const PICKER_HOLDERS = {
  connectFolder: "connect-folder",
  confirmApprovedFolder: "confirm-approved-folder",
  grantFolderCapability: "grant-folder-capability",
  importSource: "import-source",
  exportOutput: "export-output",
  saveDebugBundle: "save-debug-bundle",
  attachImage: "attach-image",
} as const;

/** What to tell a reader who tried to open a second picker. */
export const PICKER_BUSY_MESSAGE =
  "Another file or folder window is already open. Finish with it first.";

export function createNativePickerLatchStore() {
  return create<NativePickerLatchStore>()((set, get) => ({
    holder: null,
    claim: (holder) => {
      if (get().holder !== null) return false;
      set({ holder });
      return true;
    },
    // Releasing is unconditional and safe to call for a holder that never had
    // it: the picker belongs to the host, not to whichever surface asked.
    release: (holder) =>
      set((state) => (state.holder === holder ? { holder: null } : state)),
  }));
}

export const useNativePickerLatch = createNativePickerLatchStore();

/** Whether some other surface currently holds the picker. */
export function pickerHeldByAnotherSurface(
  holder: string | null,
  self: string,
): boolean {
  return holder !== null && holder !== self;
}
