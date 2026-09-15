import { create } from "zustand";
import {
  isGatewayConnection,
  type Connection,
  type GatewayConnection,
} from "../lib/connections";
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
  connections: Connection[];
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

/** The active connection, whatever kind it is, or null when none is paired. */
export function useActiveConnection(): Connection | null {
  const connections = useConnectionStore((state) => state.connections);
  const activeId = useConnectionStore((state) => state.activeId);
  return connections.find((connection) => connection.id === activeId) ?? null;
}

/**
 * The active connection when it is a gateway pairing, and null when it is a
 * standalone machine.
 *
 * Every screen that reads a gateway URL, an installation id, or a gateway
 * identity uses this rather than `useActiveConnection`, so a gateway-only
 * surface reached with a machine connection active renders its unavailable
 * state instead of dereferencing a field that is not there.
 */
export function useActiveGatewayConnection(): GatewayConnection | null {
  const connection = useActiveConnection();
  return isGatewayConnection(connection) ? connection : null;
}

/** The machine the active connection supervises, if it has attached one. */
export function useActiveMachine() {
  return useActiveConnection()?.machine ?? null;
}

export function useActiveIdentity(): GatewayIdentity | null {
  return useActiveGatewayConnection()?.identity ?? null;
}
