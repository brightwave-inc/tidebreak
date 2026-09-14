export type SecureStorage = {
  getItem(key: string): Promise<string | null>;
  setItem(key: string, value: string): Promise<void>;
  deleteItem(key: string): Promise<void>;
};

export const SESSION_STORAGE_KEY = "tidebreak.mobile.session.v1";

/**
 * The connection directory: ids, which one is active, and the install stamp
 * that catches a reinstall. Non-secret, but kept in the same store as the
 * credentials so one read answers "what exists" and one wipe clears it all.
 */
export const CONNECTION_INDEX_KEY = "tidebreak.mobile.connections.v2";

/**
 * One connection's own key. Its record holds the durable facts plus the
 * rotating refresh token — a few hundred bytes, well inside the ~2KB value
 * ceiling expo-secure-store has on Android, which is also why access tokens
 * stay in memory and why connections are not stored as one shared blob.
 */
export function connectionStorageKey(id: string): string {
  return `tidebreak.mobile.connection.${id}`;
}

export function memoryStorage(): SecureStorage {
  const map = new Map<string, string>();
  return {
    async getItem(key) {
      return map.get(key) ?? null;
    },
    async setItem(key, value) {
      map.set(key, value);
    },
    async deleteItem(key) {
      map.delete(key);
    },
  };
}

/** expo-secure-store's web module is an empty stub, so persist in memory there. */
export function storageForOs(os: string, native: SecureStorage): SecureStorage {
  return os === "web" ? memoryStorage() : native;
}
