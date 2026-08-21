import { createContext, useContext } from "react";

import type { ManagedPolicy } from "./api";

/** The open experience: what every surface assumes until the gate says
 * otherwise, so a component rendered outside it (a test, a story) reads
 * unmanaged rather than half-managed. */
const UNMANAGED: ManagedPolicy = {
  managed: false,
  source: "unmanaged",
  misconfigured: false,
  allow_local_mcp_servers: false,
};

/**
 * The resolved managed-mode policy, published by the gate that already reads
 * it.
 *
 * Surfaces that only need to know *whether* the profile is managed — the
 * settings rail, the locked panels — read it here instead of each fetching
 * `/policy` again, and there is exactly one answer in the tree at a time.
 */
export const ManagedPolicyContext = createContext<ManagedPolicy>(UNMANAGED);

export function useManagedPolicy(): ManagedPolicy {
  return useContext(ManagedPolicyContext);
}
