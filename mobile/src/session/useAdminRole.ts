/**
 * Learns whether the signed-in account administers the active gateway, and
 * caches the answer on the connection.
 *
 * The unfiltered usage read's `scope` is the only administrator signal this
 * client is given, and it arrives with a read the hub and the activity screen
 * already make — so the role is a by-product of a screen someone opened rather
 * than a probe of its own.
 *
 * Caching it matters for how the app paints. The administration group and the
 * in-screen administrator affordances gate on this, and resolving them a beat
 * after first paint would flash the surfaces in, which reads as the app
 * changing its mind about who you are. Persisted on the connection, the answer
 * is already there on the next launch; signing out of that connection deletes
 * the record and the answer with it.
 *
 * Writes only on a real change, and only from a scope that answers the
 * question — `adminFromUsageScope` returns undefined for the `filtered` scope a
 * parameterised read carries, which must not demote a cached administrator.
 *
 * What keeps this honest across a change of account is not this hook: the read
 * it learns from is filed under `consoleCacheScope`, which moves with the
 * pairing generation. A member signing in where an administrator was finds no
 * cached usage response to read a scope out of, so there is nothing here to
 * persist until the gateway has answered that account for itself.
 */

import { useEffect } from "react";
import { adminFromUsageScope } from "../lib/admin";
import { connections } from "./runtime";
import { useActiveGatewayConnection } from "./store";

export function useLearnedAdminRole(scope: string | null | undefined): void {
  const connection = useActiveGatewayConnection();
  const cached = connection?.isAdmin;
  const connectionId = connection?.id;

  useEffect(() => {
    const learned = adminFromUsageScope(scope);
    if (learned === undefined || learned === cached || !connectionId) {
      return;
    }
    // Not awaited: this is a cache write behind a screen that already has its
    // answer, and a storage failure must not take the screen down with it.
    void connections.updateActive({ isAdmin: learned }).catch(() => undefined);
  }, [scope, cached, connectionId]);
}
