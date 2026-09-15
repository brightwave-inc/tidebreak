/**
 * The credential core for a standalone machine connection (#3404).
 *
 * `TokenStore` holds a rotating OAuth refresh family and mints a short-lived
 * access token per resource. A standalone machine has no authorization server
 * at all: the operator hands over one long-lived roster token and that token
 * *is* the bearer, on every request and on every socket, until somebody
 * removes it from the machine's token file.
 *
 * So this store is the same shape with the rotation taken out. It owns one
 * secure-store key, answers `getAccessToken` from the stored token, and
 * publishes the same `onSignedOut` signal the registry already listens for —
 * which is what lets one revoked machine drop out of the connection list while
 * every other connection stays signed in.
 *
 * **What replaces rotation is `reportUnauthorized`.** A gateway connection
 * learns it is finished at the token endpoint, where a refused refresh is
 * unambiguous. A static token has no such moment: the only evidence it stopped
 * working is the machine answering `401` to an ordinary request
 * (`require_token` in `crates/tidebreak-server/src/auth.rs` answers `401` for
 * an unresolvable bearer on both REST calls and WebSocket upgrades — never
 * `403`, which that server reserves for an authenticated principal who lacks a
 * role). `MachineClient` reports exactly that status back here, and this store
 * treats it the way `TokenStore` treats `invalid_grant`: wipe the credential,
 * signal, and let the registry forget this connection alone.
 *
 * The token is never logged, never put in a URL, and never written anywhere
 * but this key.
 */

import type { MachineConnection } from "./connections";
import { connectionStorageKey, type SecureStorage } from "./storage";
import { SignedOutError } from "./tokenStore";

/**
 * One standalone connection's persisted record: the durable facts plus the
 * static token. There is no access-token cache — the token is the credential.
 */
export type StoredMachineConnection = MachineConnection & {
  staticToken: string;
};

export class StaticTokenStore {
  private record: StoredMachineConnection | null = null;
  private signedOutListeners = new Set<() => void>();
  private readonly storageKey: string;

  constructor(
    private readonly storage: SecureStorage,
    readonly connectionId: string,
  ) {
    this.storageKey = connectionStorageKey(connectionId);
  }

  onSignedOut(listener: () => void): () => void {
    this.signedOutListeners.add(listener);
    return () => {
      this.signedOutListeners.delete(listener);
    };
  }

  async hydrate(): Promise<StoredMachineConnection | null> {
    const raw = await this.storage.getItem(this.storageKey);
    if (!raw) {
      this.record = null;
      return null;
    }
    try {
      const parsed = JSON.parse(raw) as StoredMachineConnection;
      if (
        typeof parsed.staticToken !== "string" ||
        parsed.staticToken.length === 0 ||
        !parsed.machine?.baseUrl
      ) {
        await this.clear();
        return null;
      }
      this.record = { ...parsed, kind: "machine" };
    } catch {
      await this.clear();
      return null;
    }
    return this.record;
  }

  snapshot(): StoredMachineConnection | null {
    return this.record;
  }

  async replace(record: StoredMachineConnection): Promise<void> {
    this.record = record;
    await this.persist();
  }

  async update(partial: Partial<StoredMachineConnection>): Promise<void> {
    if (!this.record) {
      throw new SignedOutError("This machine connection is signed out");
    }
    this.record = { ...this.record, ...partial };
    await this.persist();
  }

  async clear(): Promise<void> {
    this.record = null;
    await this.storage.deleteItem(this.storageKey);
    for (const listener of this.signedOutListeners) {
      listener();
    }
  }

  /**
   * The bearer for this machine.
   *
   * The resource argument is the locally derived `tidebreak:<digest>` the
   * attach recorded, and it is checked rather than ignored: a caller asking
   * this connection for `control` or `control_plane` has reached a
   * gateway-only surface that should not have rendered, and answering it with
   * the machine's own credential would send a roster token to a server that
   * never issued one.
   */
  async getAccessToken(resource: string): Promise<string> {
    const record = this.record;
    if (!record) {
      throw new SignedOutError("This machine connection is signed out");
    }
    if (resource !== record.machine.resource) {
      throw new Error(
        `This connection has no gateway, so it cannot hold ${resource}.`,
      );
    }
    return record.staticToken;
  }

  /**
   * The machine refused this credential. Anything but `401` is left alone: a
   * `403` is an authorization answer about a principal the machine still
   * recognizes, and signing out on it would throw a member off a deployment
   * for opening one admin-only surface.
   */
  reportUnauthorized(status: number): void {
    if (status !== 401 || !this.record) {
      return;
    }
    void this.clear();
  }

  private async persist(): Promise<void> {
    if (!this.record) {
      await this.storage.deleteItem(this.storageKey);
      return;
    }
    await this.storage.setItem(this.storageKey, JSON.stringify(this.record));
  }
}
