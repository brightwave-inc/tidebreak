/**
 * Per-connection record of the push address last registered with a gateway.
 *
 * This exists for one moment: revoking. By the time a device is deregistered
 * — sign-out, or permission withdrawn — the OS may no longer answer
 * `getExpoPushTokenAsync` at all, so the token that must be named in the
 * DELETE has to have been written down while it was still obtainable.
 *
 * Kept in its own secure-store key rather than on the connection record so
 * that this module imports neither the registry nor the API layer: the
 * sign-out path calls it, the reconcile path calls it, and a richer dependency
 * either way would be an import cycle.
 */

import type { SecureStorage } from "../lib/storage";

function pushTokenKey(connectionId: string): string {
  return `tidebreak.mobile.push.${connectionId}`;
}

export async function readPushToken(
  storage: SecureStorage,
  connectionId: string,
): Promise<string | null> {
  try {
    return await storage.getItem(pushTokenKey(connectionId));
  } catch {
    return null;
  }
}

export async function writePushToken(
  storage: SecureStorage,
  connectionId: string,
  token: string,
): Promise<void> {
  try {
    await storage.setItem(pushTokenKey(connectionId), token);
  } catch {
    // Best-effort: a lost record only costs a later no-op revoke.
  }
}

export async function clearPushToken(
  storage: SecureStorage,
  connectionId: string,
): Promise<void> {
  try {
    await storage.deleteItem(pushTokenKey(connectionId));
  } catch {
    // Best-effort.
  }
}
