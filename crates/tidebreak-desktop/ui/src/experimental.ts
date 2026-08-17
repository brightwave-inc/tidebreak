import { create } from "zustand";

import type { ApiClient } from "./api";

/**
 * Experimental feature flags, read from `/settings` once the shell has a
 * client and rewritten by the Experimental settings panel.
 *
 * Surfaces gate on the flag *and* `loaded`: until the fetch lands the app
 * behaves as opted out, so an experimental surface never flashes into a rail
 * it is about to leave. There is exactly one answer in the tree at a time —
 * panels that toggle a flag write it back here instead of refetching.
 */

type ExperimentalFlagsStore = {
  loaded: boolean;
  codeModeEnabled: boolean;
  refresh: (client: ApiClient) => Promise<void>;
  setCodeModeEnabled: (enabled: boolean) => void;
};

export const useExperimentalFlags = create<ExperimentalFlagsStore>()((set) => ({
  loaded: false,
  codeModeEnabled: false,
  refresh: async (client) => {
    try {
      const settings = await client.getSettings();
      set({ loaded: true, codeModeEnabled: settings.code_mode_enabled });
    } catch {
      // A failed read keeps the opted-out default; the flags load again with
      // the next shell boot, and nothing user-facing breaks meanwhile.
      set({ loaded: true });
    }
  },
  setCodeModeEnabled: (enabled) => set({ codeModeEnabled: enabled }),
}));
