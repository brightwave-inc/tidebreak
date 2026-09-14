import { create } from "zustand";
import type { GatewayConnection } from "../lib/connections";
import type { RegistrySnapshot } from "../lib/connectionRegistry";
import type { GatewayIdentity } from "../lib/types";

/**
 * The connections the UI renders, mirrored from the registry.
 *
 * The registry is the authority — it owns persistence and credentials — and
 * this store is the React-visible projection of its snapshot, so a screen
 * never has to await storage to paint.
 */
export type ConnectionState = {
  hydrated: boolean;
  connections: GatewayConnection[];
  activeId: string | null;
  apply: (snapshot: RegistrySnapshot) => void;
  setHydrated: (snapshot: RegistrySnapshot) => void;
};

export const useConnectionStore = create<ConnectionState>((set) => ({
  hydrated: false,
  connections: [],
  activeId: null,
  apply: (snapshot) =>
    set({
      connections: snapshot.connections,
      activeId: snapshot.activeId,
    }),
  setHydrated: (snapshot) =>
    set({
      hydrated: true,
      connections: snapshot.connections,
      activeId: snapshot.activeId,
    }),
}));

/** The active connection, or null when none is paired. */
export function useActiveConnection(): GatewayConnection | null {
  const connections = useConnectionStore((state) => state.connections);
  const activeId = useConnectionStore((state) => state.activeId);
  return connections.find((connection) => connection.id === activeId) ?? null;
}

/** The machine the active connection supervises, if it has attached one. */
export function useActiveMachine() {
  return useActiveConnection()?.machine ?? null;
}

export function useActiveIdentity(): GatewayIdentity | null {
  return useActiveConnection()?.identity ?? null;
}
