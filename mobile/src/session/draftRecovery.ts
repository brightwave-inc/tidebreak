import { create } from "zustand";

export type SessionDraftRecovery = {
  draft: string;
  error: string;
};

type SessionDraftRecoveryState = {
  bySession: Record<string, SessionDraftRecovery>;
  offer: (sessionId: string, recovery: SessionDraftRecovery) => void;
  consume: (sessionId: string) => void;
  reset: () => void;
};

export const useSessionDraftRecoveryStore =
  create<SessionDraftRecoveryState>((set) => ({
    bySession: {},
    offer: (sessionId, recovery) =>
      set((state) => ({
        bySession: { ...state.bySession, [sessionId]: recovery },
      })),
    consume: (sessionId) =>
      set((state) => {
        if (!(sessionId in state.bySession)) return state;
        const bySession = { ...state.bySession };
        delete bySession[sessionId];
        return { bySession };
      }),
    reset: () => set({ bySession: {} }),
  }));

