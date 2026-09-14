import { Platform } from "react-native";
import * as Application from "expo-application";
import * as SecureStore from "expo-secure-store";
import { ConnectionRegistry } from "../lib/connectionRegistry";
import { fetchTokenHttp } from "../lib/gateway";
import { storageForOs, type SecureStorage } from "../lib/storage";

const expoStorage: SecureStorage = {
  getItem: (key) => SecureStore.getItemAsync(key),
  setItem: (key, value) => SecureStore.setItemAsync(key, value),
  deleteItem: (key) => SecureStore.deleteItemAsync(key),
};

/**
 * When this install was installed.
 *
 * iOS Keychain entries outlive the app that wrote them, so a reinstall can
 * find the previous install's refresh tokens. The install time is the cheapest
 * marker that changes exactly then — it lives outside the keychain, needs no
 * extra dependency, and a platform that cannot answer (web) reports null,
 * which the registry reads as "cannot tell" rather than "wipe".
 */
async function installTimeMs(): Promise<number | null> {
  try {
    const installedAt: Date | null = await Application.getInstallationTimeAsync();
    const value = installedAt?.getTime();
    return typeof value === "number" && Number.isFinite(value) ? value : null;
  } catch {
    return null;
  }
}

export const connections = new ConnectionRegistry({
  storage: storageForOs(Platform.OS, expoStorage),
  http: fetchTokenHttp(),
  installTimeMs,
});

/**
 * The active connection's credential core. Every screen mints through this
 * rather than a module-level singleton, so switching connections switches
 * which refresh family the next request rotates.
 */
export function activeTokens() {
  return connections.activeTokens();
}
