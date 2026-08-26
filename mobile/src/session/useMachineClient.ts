import { useMemo } from "react";
import { MachineClient } from "../lib/machine";
import { tokenStore } from "./runtime";
import { useSessionStore } from "./store";

export function useMachineClient(): MachineClient | null {
  const machine = useSessionStore((state) => state.session?.machine);
  return useMemo(() => {
    if (!machine) return null;
    return new MachineClient({
      baseUrl: machine.baseUrl,
      resource: machine.resource,
      tokens: tokenStore,
    });
  }, [machine?.baseUrl, machine?.resource]);
}
