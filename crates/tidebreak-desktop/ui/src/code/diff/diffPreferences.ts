import { create } from "zustand";

/**
 * How the reader likes to read a diff. Only the layout is remembered: hiding
 * whitespace is a question about one diff, and a remembered "hide" would
 * quietly keep real changes off screen in the next one.
 */
export type DiffLayout = "unified" | "split";

const STORAGE_KEY = "tidebreak.code-diff-layout";

function readLayout(): DiffLayout {
  try {
    return window.localStorage.getItem(STORAGE_KEY) === "split"
      ? "split"
      : "unified";
  } catch {
    return "unified";
  }
}

function writeLayout(layout: DiffLayout): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, layout);
  } catch {
    // Preference persistence is best-effort.
  }
}

type DiffPreferences = {
  layout: DiffLayout;
  setLayout: (layout: DiffLayout) => void;
};

export const useDiffPreferences = create<DiffPreferences>()((set) => ({
  layout: typeof window === "undefined" ? "unified" : readLayout(),
  setLayout: (layout) => {
    writeLayout(layout);
    set({ layout });
  },
}));

/**
 * The narrowest pane that still shows two columns of code worth reading.
 * Below it a split preference draws unified, the way an editor's side-by-side
 * diff folds to one column in a narrow window.
 */
export const SPLIT_MIN_WIDTH = 560;
