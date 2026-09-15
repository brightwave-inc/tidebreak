/**
 * Machine clients, bound to the connection that built them.
 *
 * A `MachineClient` carries two consequences for a credential: it mints one to
 * send, and it learns from the response whether that credential still works.
 * Both have to land on the connection whose token actually went on the wire,
 * and an active-connection lookup cannot promise that — it answers about the
 * moment it is asked, not about the moment the request was made.
 *
 * The gap between those two moments is real and ordinary. Every live surface
 * polls, and a poll in flight survives the user tapping a different connection
 * in the list. Resolving the 401 consequence at *response* time would then wipe
 * whichever machine is active by the time the answer lands — a working roster
 * token that was never on that wire — while the machine that was actually
 * refused stays signed in, still polling, still being refused.
 *
 * So the connection id is captured when the client is built and every lookup
 * goes through `tokensFor(id)`. This is the shape the push work already uses
 * for the same class of problem: a notification action mints against the
 * connection its payload names (`push/decisionResponses.ts`), never against
 * whichever one happens to be active, so a tray press cannot re-point the app.
 *
 * A client whose connection has since been signed out mints nothing and says
 * so. Borrowing another connection's credential to finish an orphaned request
 * is exactly the confusion this module exists to prevent.
 */

import { MachineClient, type MachineClientOptions, type TokenSource } from "./machine";
import { StaticTokenStore } from "./machineTokenStore";
import { SignedOutError } from "./tokenStore";
import type { ConnectionCredentials } from "./connectionRegistry";
import type { AttachedMachine } from "./types";

/** The part of the registry a machine client needs. */
export type CredentialLookup = {
  tokensFor(id: string): ConnectionCredentials | null;
};

/**
 * A token source pinned to one connection for the life of the client.
 *
 * `reportStatus` is routed to the same store that minted, which is what makes
 * a late refusal land on the right connection. Only a static token acts on it:
 * a gateway connection's `401` from a machine is an expired minted token, and
 * its refresh family — not this signal — decides whether the session is over.
 */
export function boundTokenSource(
  registry: CredentialLookup,
  connectionId: string,
): TokenSource {
  const storeFor = (): ConnectionCredentials => {
    const store = registry.tokensFor(connectionId);
    if (!store) {
      throw new SignedOutError("That connection is no longer signed in");
    }
    return store;
  };
  return {
    getAccessToken: (resource) => storeFor().getAccessToken(resource),
    reportStatus: (status) => {
      // Only a static token acts on this. A `TokenStore` deliberately has no
      // such method, for the reason in the doc comment above.
      const store = registry.tokensFor(connectionId);
      if (store instanceof StaticTokenStore) {
        store.reportUnauthorized(status);
      }
    },
  };
}

export type MachineClientOverrides = Pick<
  MachineClientOptions,
  "fetchImpl" | "webSocket"
>;

/** A client for `machine`, authenticated as `connectionId` and nobody else. */
export function machineClientFor(
  registry: CredentialLookup,
  connectionId: string,
  machine: AttachedMachine,
  overrides: MachineClientOverrides = {},
): MachineClient {
  return new MachineClient({
    baseUrl: machine.baseUrl,
    resource: machine.resource,
    tokens: boundTokenSource(registry, connectionId),
    ...overrides,
  });
}
