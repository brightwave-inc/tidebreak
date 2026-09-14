import { useMemo } from "react";
import { MachineClient } from "../lib/machine";
import { connections } from "./runtime";
import { useActiveMachine } from "./store";

// One client instance per attached machine, shared across every mount, so
// consumers that key shared resources by client identity (the updates feed)
// coalesce instead of each opening their own.
let cached: {
  baseUrl: string;
  resource: string;
  client: MachineClient;
} | null = null;

function machineClientFor(machine: {
  baseUrl: string;
  resource: string;
}): MachineClient {
  if (
    !cached ||
    cached.baseUrl !== machine.baseUrl ||
    cached.resource !== machine.resource
  ) {
    cached = {
      baseUrl: machine.baseUrl,
      resource: machine.resource,
      client: new MachineClient({
        baseUrl: machine.baseUrl,
        resource: machine.resource,
        // Resolved per call, not captured: switching connections switches
        // which refresh family mints this machine's tokens.
        tokens: {
          getAccessToken: (resource) =>
            connections.activeTokens().getAccessToken(resource),
        },
      }),
    };
  }
  return cached.client;
}

export function useMachineClient(): MachineClient | null {
  const machine = useActiveMachine();
  return useMemo(() => {
    if (!machine) return null;
    return machineClientFor(machine);
  }, [machine?.baseUrl, machine?.resource]);
}
