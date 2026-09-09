import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
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
  // listedSessions builds a fresh array, so the raw selector returns a new
  // reference on every call — under useSyncExternalStore that re-renders
  // forever ("Maximum update depth exceeded"). useShallow keeps the previous
  // array while its elements are unchanged.
  return useUpdatesStore(useShallow((state) => listedSessions(state)));
}

export function useHasSnapshot(): boolean {
  return useUpdatesStore((state) => state.snapshotReceived);
}
