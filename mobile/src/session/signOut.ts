/**
 * Signing out of one connection, in the order that matters.
 *
 * Deregistering the push device needs a live `control` bearer minted from that
 * connection's refresh family, so it has to happen *before* the credential is
 * deleted. The reverse order silently leaves a dead address on the gateway
 * pointed at a phone that has signed out — it keeps sending, and the app keeps
 * showing notifications for an account it can no longer open.
 *
 * Deregistration is best-effort and time-bounded inside `deregisterConnection`:
 * an unreachable gateway must never be able to trap a user in a session they
 * asked to leave.
 *
 * A standalone machine connection (#3404) has no gateway to deregister from —
 * push is a gateway service — so signing one out is the credential wipe alone:
 * its roster token is deleted from the secure store and nothing else is
 * touched.
 */

import { isGatewayConnection } from "../lib/connections";
import { deregisterConnection } from "../push/registration";
import { connections } from "./runtime";

export async function signOutConnection(id: string): Promise<void> {
  const connection = connections
    .list()
    .find((candidate) => candidate.id === id);
  if (isGatewayConnection(connection)) {
    await deregisterConnection(id, connection.gatewayUrl);
  }
  await connections.remove(id);
}

/** Signs out of whichever connection is active, if any. */
export async function signOutActiveConnection(): Promise<void> {
  const active = connections.active();
  if (active) {
    await signOutConnection(active.id);
  }
}
