import { Platform } from "react-native";
import * as Application from "expo-application";
import * as SecureStore from "expo-secure-store";
import {
  ConnectionRegistry,
  type RegistrySnapshot,
} from "../lib/connectionRegistry";
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

export const secureStorage: SecureStorage = storageForOs(
  Platform.OS,
  expoStorage,
);

export const connections = new ConnectionRegistry({
  storage: secureStorage,
  http: fetchTokenHttp(),
  installTimeMs,
});

/**
 * Hydrates the registry once per process, whatever reached it first.
 *
 * The root layout does this on mount, but a notification action pressed while
 * the app is killed launches the bundle with no React tree at all — so the
 * background executor has to be able to ask for the same thing and get the
 * same single hydrate rather than a second, racing one.
 */
let hydration: Promise<RegistrySnapshot> | null = null;

export function hydrateConnections(): Promise<RegistrySnapshot> {
  hydration ??= connections.hydrate();
  return hydration;
}

/**
 * The active connection's credential core. Every screen mints through this
 * rather than a module-level singleton, so switching connections switches
 * which refresh family the next request rotates.
 */
export function activeTokens() {
  return connections.activeTokens();
}
