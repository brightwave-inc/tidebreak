import { create } from "zustand";
import type { GatewayIdentity, PersistedSession } from "../lib/types";

export type SessionState = {
  hydrated: boolean;
  session: PersistedSession | null;
  identity: GatewayIdentity | null;
  setHydrated: (session: PersistedSession | null) => void;
  setSession: (session: PersistedSession | null) => void;
  setIdentity: (identity: GatewayIdentity | null) => void;
  signOutLocal: () => void;
};

export const useSessionStore = create<SessionState>((set) => ({
  hydrated: false,
  session: null,
  identity: null,
  setHydrated: (session) =>
    set({
      hydrated: true,
      session,
      identity: session?.identity ?? null,
    }),
  setSession: (session) =>
    set({
      session,
      identity: session?.identity ?? null,
    }),
  setIdentity: (identity) => set({ identity }),
  signOutLocal: () => set({ session: null, identity: null }),
}));
