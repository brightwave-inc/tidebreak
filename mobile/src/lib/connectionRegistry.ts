/**
 * The connection registry: every connection this phone holds, which one is
 * active, and one `TokenStore` per connection.
 *
 * It owns three things the screens must not each reinvent.
 *
 * **Plurality.** Connections are a list with an active member. Adding,
 * switching, and removing are index writes; a removal takes that connection's
 * credential with it and leaves every other connection signed in.
 *
 * **Migration.** Builds before this model kept one session under
 * `tidebreak.mobile.session.v1`. That blob is adopted as a gateway connection
 * on first hydrate and deleted, so an existing install keeps its pairing, its
 * machine, and its rotating refresh token across the upgrade.
 *
 * **Reinstall hygiene.** iOS Keychain entries survive an uninstall, so a fresh
 * install can find a previous install's credentials. The index records the
 * install time; a different one means wipe everything before reading it.
 */

import {
  activeAfterRemoval,
  emptyIndex,
  gatewayConnectionId,
  reinstalled,
  withConnectionId,
  type ConnectionIndex,
  type GatewayConnection,
} from "./connections";
import {
  CONNECTION_INDEX_KEY,
  SESSION_STORAGE_KEY,
  connectionStorageKey,
  type SecureStorage,
} from "./storage";
import { SignedOutError, TokenStore, type TokenHttp } from "./tokenStore";
import type { PersistedSession } from "./types";

/** One connection as the UI sees it: durable facts, no credential. */
export type ConnectionSummary = GatewayConnection;

export type RegistrySnapshot = {
  connections: ConnectionSummary[];
  activeId: string | null;
};

export type RegistryDeps = {
  storage: SecureStorage;
  http: TokenHttp;
  /**
   * When the running install was installed, in epoch milliseconds, or null
   * when the platform cannot say. Only a known-different value triggers the
   * stale-credential wipe.
   */
  installTimeMs?: () => Promise<number | null>;
  /** Clock for `addedAt`; injected so tests can pin it. */
  now?: () => Date;
};

/** What pairing knows about a new gateway connection. */
export type NewGatewayConnection = {
  gatewayUrl: string;
  refreshToken: string;
  installationId?: string;
  machinePrefillUrl?: string;
  grantedScope?: string;
};

export class ConnectionRegistry {
  private index: ConnectionIndex = emptyIndex();
  private stores = new Map<string, TokenStore>();
  private records = new Map<string, GatewayConnection>();
  private listeners = new Set<(snapshot: RegistrySnapshot) => void>();
  private hydrated = false;

  constructor(private readonly deps: RegistryDeps) {}

  /** Notified whenever the set of connections or the active one changes. */
  onChange(listener: (snapshot: RegistrySnapshot) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  async hydrate(): Promise<RegistrySnapshot> {
    const installTime = this.deps.installTimeMs
      ? await this.deps.installTimeMs().catch(() => null)
      : null;
    let index = await this.readIndex();
    if (index && reinstalled(index.installTimeMs, installTime)) {
      await this.wipeAll(index);
      index = null;
    }
    if (!index) {
      index = await this.migrateLegacySession(installTime);
    }
    this.index = {
      ...index,
      ...(installTime === null ? {} : { installTimeMs: installTime }),
    };
    const live: string[] = [];
    for (const id of this.index.ids) {
      const store = this.storeFor(id);
      const record = await store.hydrate();
      if (!record) {
        this.stores.delete(id);
        continue;
      }
      live.push(id);
      this.records.set(id, publicRecord(record));
    }
    this.index = {
      ...this.index,
      ids: live,
      activeId:
        this.index.activeId && live.includes(this.index.activeId)
          ? this.index.activeId
          : (live[live.length - 1] ?? null),
    };
    await this.writeIndex();
    this.hydrated = true;
    return this.snapshot();
  }

  snapshot(): RegistrySnapshot {
    return {
      connections: this.index.ids.flatMap((id) => {
        const record = this.records.get(id);
        return record ? [record] : [];
      }),
      activeId: this.index.activeId,
    };
  }

  list(): ConnectionSummary[] {
    return this.snapshot().connections;
  }

  active(): ConnectionSummary | null {
    const id = this.index.activeId;
    return id ? (this.records.get(id) ?? null) : null;
  }

  /** The credential core for one connection, or null when it is unknown. */
  tokensFor(id: string): TokenStore | null {
    return this.stores.get(id) ?? null;
  }

  /**
   * The active connection's credential core. Throws rather than returning null
   * so a caller that reached a signed-in surface cannot silently mint nothing.
   */
  activeTokens(): TokenStore {
    const id = this.index.activeId;
    const store = id ? this.stores.get(id) : null;
    if (!store) {
      throw new SignedOutError("No gateway connection is active");
    }
    return store;
  }

  /**
   * Adds a freshly paired gateway and makes it active. Pairing the same
   * deployment again replaces its record — the id is derived from the
   * installation — so a re-pair refreshes the credential instead of stacking a
   * second live refresh family for one gateway.
   *
   * Because that id is a deployment's name and not an account's, the record is
   * rebuilt from scratch rather than merged: everything the previous sign-in
   * learned about *who* was signed in (`isAdmin`, and the identity behind it)
   * has to go, and `pairingGeneration` moves on so the cached reads filed
   * under the old one cannot be addressed by the new session either.
   */
  async addGateway(input: NewGatewayConnection): Promise<GatewayConnection> {
    const id = gatewayConnectionId(input.gatewayUrl, input.installationId);
    const now = this.deps.now?.() ?? new Date();
    const existing = this.records.get(id);
    const record: GatewayConnection = {
      id,
      kind: "gateway",
      gatewayUrl: input.gatewayUrl,
      addedAt: existing?.addedAt ?? now.toISOString(),
      pairingGeneration: (existing?.pairingGeneration ?? 0) + 1,
      ...(input.installationId ? { installationId: input.installationId } : {}),
      ...(input.machinePrefillUrl
        ? { machinePrefillUrl: input.machinePrefillUrl }
        : {}),
      ...(input.grantedScope ? { grantedScope: input.grantedScope } : {}),
    };
    const store = this.storeFor(id);
    await store.replace({
      ...record,
      refreshToken: input.refreshToken,
      accessTokens: [],
    });
    this.records.set(id, record);
    this.index = {
      ...this.index,
      ids: withConnectionId(this.index.ids, id),
      activeId: id,
    };
    await this.writeIndex();
    this.emit();
    return record;
  }

  /** Persists a change to the active connection's durable facts. */
  async updateActive(
    partial: Partial<Omit<GatewayConnection, "id" | "kind">>,
  ): Promise<GatewayConnection | null> {
    const id = this.index.activeId;
    if (!id) {
      return null;
    }
    const store = this.stores.get(id);
    if (!store) {
      return null;
    }
    await store.update(partial);
    const record = store.snapshot();
    if (!record) {
      return null;
    }
    const summary = publicRecord(record);
    this.records.set(id, summary);
    this.emit();
    return summary;
  }

  async setActive(id: string): Promise<void> {
    if (!this.index.ids.includes(id)) {
      return;
    }
    this.index = { ...this.index, activeId: id };
    await this.writeIndex();
    this.emit();
  }

  /**
   * Signs out of one connection and forgets it: its credential is deleted,
   * every other connection is untouched, and the newest survivor becomes
   * active.
   */
  async remove(id: string): Promise<void> {
    const store = this.stores.get(id);
    if (store) {
      await store.clear();
    } else {
      await this.deps.storage.deleteItem(connectionStorageKey(id));
    }
    await this.forget(id);
  }

  /** Signs out of the active connection. */
  async removeActive(): Promise<void> {
    const id = this.index.activeId;
    if (id) {
      await this.remove(id);
    }
  }

  /**
   * Drops one id from the directory. Separate from `remove` because a
   * `TokenStore` also clears itself when the gateway revokes its refresh
   * family, and that path must reach the same place.
   */
  private async forget(id: string): Promise<void> {
    if (!this.index.ids.includes(id)) {
      // A `TokenStore` clearing itself already ran this path; removing the
      // same connection twice must not publish a second change.
      return;
    }
    const activeId = activeAfterRemoval(this.index.ids, this.index.activeId, id);
    this.index = {
      ...this.index,
      ids: this.index.ids.filter((existing) => existing !== id),
      activeId,
    };
    this.stores.delete(id);
    this.records.delete(id);
    await this.writeIndex();
    this.emit();
  }

  private storeFor(id: string): TokenStore {
    const existing = this.stores.get(id);
    if (existing) {
      return existing;
    }
    const store = new TokenStore(this.deps.storage, this.deps.http, id);
    store.onSignedOut(() => {
      // A revoked family signs out exactly this connection. Nothing awaits
      // this: the listener runs inside the store's own wipe.
      if (this.index.ids.includes(id)) {
        void this.forget(id);
      }
    });
    this.stores.set(id, store);
    return store;
  }

  private async readIndex(): Promise<ConnectionIndex | null> {
    const raw = await this.deps.storage.getItem(CONNECTION_INDEX_KEY);
    if (!raw) {
      return null;
    }
    try {
      const parsed = JSON.parse(raw) as ConnectionIndex;
      if (!Array.isArray(parsed.ids)) {
        return null;
      }
      return {
        version: 2,
        ids: parsed.ids.filter((id) => typeof id === "string"),
        activeId: typeof parsed.activeId === "string" ? parsed.activeId : null,
        ...(typeof parsed.installTimeMs === "number"
          ? { installTimeMs: parsed.installTimeMs }
          : {}),
      };
    } catch {
      return null;
    }
  }

  private async writeIndex(): Promise<void> {
    await this.deps.storage.setItem(
      CONNECTION_INDEX_KEY,
      JSON.stringify(this.index),
    );
  }

  private async wipeAll(index: ConnectionIndex): Promise<void> {
    for (const id of index.ids) {
      await this.deps.storage.deleteItem(connectionStorageKey(id));
    }
    await this.deps.storage.deleteItem(SESSION_STORAGE_KEY);
    await this.deps.storage.deleteItem(CONNECTION_INDEX_KEY);
    this.stores.clear();
    this.records.clear();
  }

  /**
   * Adopts the single-session blob an older build wrote.
   *
   * Runs only when no directory exists, which is also the only moment the
   * reinstall check cannot fire: an install upgrading into this model has no
   * recorded install time to compare against, so its blob is migrated rather
   * than wiped. From the first hydrate onwards the stamp exists and a later
   * reinstall is caught.
   */
  private async migrateLegacySession(
    installTimeMs: number | null,
  ): Promise<ConnectionIndex> {
    const fresh = emptyIndex(installTimeMs ?? undefined);
    const raw = await this.deps.storage.getItem(SESSION_STORAGE_KEY);
    if (!raw) {
      return fresh;
    }
    let legacy: PersistedSession;
    try {
      legacy = JSON.parse(raw) as PersistedSession;
    } catch {
      await this.deps.storage.deleteItem(SESSION_STORAGE_KEY);
      return fresh;
    }
    if (
      typeof legacy.gatewayUrl !== "string" ||
      typeof legacy.refreshToken !== "string"
    ) {
      await this.deps.storage.deleteItem(SESSION_STORAGE_KEY);
      return fresh;
    }
    const id = gatewayConnectionId(legacy.gatewayUrl, legacy.installationId);
    const migrated: GatewayConnection = {
      id,
      kind: "gateway",
      gatewayUrl: legacy.gatewayUrl,
      addedAt: (this.deps.now?.() ?? new Date()).toISOString(),
      ...(legacy.installationId
        ? { installationId: legacy.installationId }
        : {}),
      ...(legacy.machinePrefillUrl
        ? { machinePrefillUrl: legacy.machinePrefillUrl }
        : {}),
      ...(legacy.machine ? { machine: legacy.machine } : {}),
      ...(legacy.identity ? { identity: legacy.identity } : {}),
      // No granted scope: the blob predates recording one, and an unknown
      // grant reads as the baseline authority rather than as console reach.
    };
    await this.deps.storage.setItem(
      connectionStorageKey(id),
      JSON.stringify({
        ...migrated,
        refreshToken: legacy.refreshToken,
        accessTokens: [],
      }),
    );
    await this.deps.storage.deleteItem(SESSION_STORAGE_KEY);
    return { ...fresh, ids: [id], activeId: id };
  }

  private emit(): void {
    if (!this.hydrated) {
      return;
    }
    const snapshot = this.snapshot();
    for (const listener of this.listeners) {
      listener(snapshot);
    }
  }
}

/** A stored record without its credential. */
function publicRecord(record: GatewayConnection & { refreshToken?: string }): GatewayConnection {
  const { refreshToken: _refreshToken, ...rest } = record as GatewayConnection & {
    refreshToken?: string;
    accessTokens?: unknown;
  };
  const { accessTokens: _accessTokens, ...summary } = rest;
  return summary;
}
