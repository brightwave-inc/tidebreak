import { create } from "zustand";
import type { SessionDigest as CodeSessionDigest } from "../generated/wire";
import {
  EMPTY_UPDATES,
  listedSessions,
  reduceUpdates,
  type UpdatesAction,
  type UpdatesState,
} from "../lib/updates";

export type UpdatesStore = UpdatesState & {
  apply: (action: UpdatesAction) => void;
  reset: () => void;
};

export const useUpdatesStore = create<UpdatesStore>((set) => ({
  ...EMPTY_UPDATES,
  apply: (action) => set((state) => reduceUpdates(state, action)),
  reset: () => set({ ...EMPTY_UPDATES }),
}));

export function useListedSessions(): CodeSessionDigest[] {
  return useUpdatesStore((state) => listedSessions(state));
}

export function useHasSnapshot(): boolean {
  return useUpdatesStore((state) => state.snapshotReceived);
}
