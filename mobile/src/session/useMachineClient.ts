import { useMemo } from "react";
import type { MachineClient } from "../lib/machine";
import { machineClientFor } from "../lib/machineClients";
import { connections } from "./runtime";
import { useActiveConnection } from "./store";

/**
 * The machine client for the active connection.
 *
 * One instance per (connection, machine), shared across every mount, so
 * consumers that key shared resources by client identity — the updates feed —
 * coalesce instead of each opening their own socket.
 *
 * The connection id is part of the key, not just the machine's URL, because
 * the client is bound to the connection that built it (`machineClients.ts`):
 * two connections reaching the same machine authenticate as different
 * principals, and a client handed the wrong one would mint from the wrong
 * credential and report its refusals to the wrong connection.
 */
let cached: {
  connectionId: string;
  baseUrl: string;
  resource: string;
  client: MachineClient;
} | null = null;

export function useMachineClient(): MachineClient | null {
  const connection = useActiveConnection();
  const connectionId = connection?.id ?? null;
  const machine = connection?.machine ?? null;
  return useMemo(() => {
    if (!connectionId || !machine) {
      return null;
    }
    if (
      !cached ||
      cached.connectionId !== connectionId ||
      cached.baseUrl !== machine.baseUrl ||
      cached.resource !== machine.resource
    ) {
      cached = {
        connectionId,
        baseUrl: machine.baseUrl,
        resource: machine.resource,
        client: machineClientFor(connections, connectionId, machine),
      };
    }
    return cached.client;
  }, [connectionId, machine?.baseUrl, machine?.resource]);
}
