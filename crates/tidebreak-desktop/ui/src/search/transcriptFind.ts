import { create } from "zustand";

/**
 * Which open transcript Cmd+F finds in.
 *
 * The shortcut lives in the shell, which knows no transcript. Each
 * transcript that can find registers here while it is mounted; the one
 * registered or focused last is the one the reader is in, and it answers the
 * shortcut. With none registered the shell declines the key.
 */
type TranscriptFindStore = {
  /** Mounted hosts, the active one last. */
  hosts: readonly string[];
  /** The host asked to open its find bar, and a counter to tell asks apart. */
  openRequest: { hostId: string; nonce: number } | null;
  register: (hostId: string) => () => void;
  activate: (hostId: string) => void;
  /** Open the active host's find bar; `false` when no transcript is up. */
  requestOpen: () => boolean;
};

let nonce = 0;

export const useTranscriptFindStore = create<TranscriptFindStore>(
  (set, get) => ({
    hosts: [],
    openRequest: null,
    register: (hostId) => {
      set((state) => ({
        hosts: [...state.hosts.filter((id) => id !== hostId), hostId],
      }));
      return () =>
        set((state) => ({
          hosts: state.hosts.filter((id) => id !== hostId),
          openRequest:
            state.openRequest?.hostId === hostId ? null : state.openRequest,
        }));
    },
    activate: (hostId) => {
      const { hosts } = get();
      if (!hosts.includes(hostId) || hosts.at(-1) === hostId) return;
      set({ hosts: [...hosts.filter((id) => id !== hostId), hostId] });
    },
    requestOpen: () => {
      const hostId = get().hosts.at(-1);
      if (!hostId) return false;
      nonce += 1;
      set({ openRequest: { hostId, nonce } });
      return true;
    },
  }),
);
