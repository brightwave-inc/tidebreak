import {
  CLIENT_ID,
  EXPIRY_LEEWAY_MS,
  type CachedAccessToken,
  type TokenResponse,
} from "./types";
import { connectionStorageKey, type SecureStorage } from "./storage";
import type { GatewayConnection } from "./connections";
import { isAllowedResource } from "./resource";

export class SignedOutError extends Error {
  constructor(message = "The gateway session is no longer valid") {
    super(message);
    this.name = "SignedOutError";
  }
}

/**
 * The gateway refused to mint this resource for this client, and the session
 * is fine.
 *
 * Distinct from `SignedOutError` because the wire shape is nearly identical: a
 * confined client asking for a resource its registration does not name gets
 * HTTP 400 `invalid_resource` (mg `cli_auth.rs`), the same status a revoked
 * refresh family returns. Treating that as a sign-out would mean one console
 * screen probing a gateway that has not been widened yet
 * (brightwave-inc/model-gateway#2044) throws the user out of a working machine
 * session. The surface that asked degrades; the pairing stands.
 */
export class ResourceRefusedError extends Error {
  readonly resource: string;

  constructor(resource: string) {
    super(`The gateway does not issue ${resource} for this client.`);
    this.name = "ResourceRefusedError";
    this.resource = resource;
  }
}

export type TokenHttp = {
  postForm(
    url: string,
    body: Record<string, string>,
  ): Promise<{ status: number; json: unknown }>;
};

/**
 * One connection's persisted record: the durable facts plus the rotating
 * refresh token. `accessTokens` is a memory-only cache and is stripped on
 * every write.
 */
export type StoredConnection = GatewayConnection & {
  refreshToken: string;
  accessTokens: CachedAccessToken[];
};

/**
 * The credential core for exactly one connection.
 *
 * Every instance owns its own secure-store key, its own refresh queue, and its
 * own sign-out listeners, so two paired gateways rotate independently and
 * signing out of one leaves the other's session untouched.
 */
export class TokenStore {
  private record: StoredConnection | null = null;
  private refreshTail: Promise<void> = Promise.resolve();
  private inflight = new Map<string, Promise<string>>();
  private signedOutListeners = new Set<() => void>();
  private readonly storageKey: string;

  constructor(
    private readonly storage: SecureStorage,
    private readonly http: TokenHttp,
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

  async hydrate(): Promise<StoredConnection | null> {
    const raw = await this.storage.getItem(this.storageKey);
    if (!raw) {
      this.record = null;
      return null;
    }
    try {
      const parsed = JSON.parse(raw) as StoredConnection;
      if (typeof parsed.refreshToken !== "string" || !parsed.gatewayUrl) {
        await this.clear();
        return null;
      }
      // Access tokens are a memory-only cache; drop any a previous build
      // wrote so a stale token is never trusted across launches.
      this.record = { ...parsed, kind: "gateway", accessTokens: [] };
    } catch {
      await this.clear();
      return null;
    }
    return this.record;
  }

  snapshot(): StoredConnection | null {
    return this.record;
  }

  async replace(record: StoredConnection): Promise<void> {
    this.record = record;
    await this.persist();
  }

  async update(partial: Partial<StoredConnection>): Promise<void> {
    if (!this.record) {
      throw new SignedOutError();
    }
    this.record = { ...this.record, ...partial };
    await this.persist();
  }

  async clear(): Promise<void> {
    // Ride the refresh queue so a refresh already in flight finishes (or
    // fails) before the wipe: otherwise its persist() could write a fresh
    // refresh token back to storage after sign-out deleted it.
    const run = this.refreshTail.then(() => this.clearNow());
    this.refreshTail = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  private async clearNow(): Promise<void> {
    this.record = null;
    this.inflight.clear();
    await this.storage.deleteItem(this.storageKey);
    for (const listener of this.signedOutListeners) {
      listener();
    }
  }

  async getAccessToken(resource: string): Promise<string> {
    if (!isAllowedResource(resource)) {
      throw new Error(`Refusing to mint an unsupported resource: ${resource}`);
    }
    const cached = this.freshAccessToken(resource);
    if (cached) {
      return cached;
    }
    const existing = this.inflight.get(resource);
    if (existing) {
      return existing;
    }
    const minted = this.enqueueRefresh(resource).finally(() => {
      this.inflight.delete(resource);
    });
    this.inflight.set(resource, minted);
    return minted;
  }

  private freshAccessToken(resource: string): string | null {
    const hit = this.record?.accessTokens.find(
      (token) => token.resource === resource,
    );
    if (!hit) {
      return null;
    }
    if (hit.expiresAtMs - EXPIRY_LEEWAY_MS <= Date.now()) {
      return null;
    }
    return hit.accessToken;
  }

  private enqueueRefresh(resource: string): Promise<string> {
    const run = this.refreshTail.then(() => this.refreshForResource(resource));
    this.refreshTail = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  private async refreshForResource(resource: string): Promise<string> {
    const stillFresh = this.freshAccessToken(resource);
    if (stillFresh) {
      return stillFresh;
    }
    const record = this.record;
    if (!record) {
      throw new SignedOutError();
    }
    const tokens = await this.requestRefresh(record, resource);
    const cached: CachedAccessToken = {
      resource,
      accessToken: tokens.access_token,
      expiresAtMs: Date.now() + tokens.expires_in * 1000,
    };
    const others = record.accessTokens.filter(
      (token) => token.resource !== resource,
    );
    this.record = {
      ...record,
      refreshToken: tokens.refresh_token,
      // The granted scope is carried, never re-read: a rotation's response
      // describes the resource it minted, so only sign-in can answer about
      // the session's authority.
      accessTokens: [...others, cached],
    };
    await this.persist();
    return tokens.access_token;
  }

  private async requestRefresh(
    record: StoredConnection,
    resource: string,
  ): Promise<TokenResponse> {
    const result = await this.http.postForm(`${record.gatewayUrl}/oauth/token`, {
      grant_type: "refresh_token",
      refresh_token: record.refreshToken,
      client_id: CLIENT_ID,
      resource,
    });
    if (result.status === 400 && errorCode(result.json) === "invalid_resource") {
      // The client may not hold this resource here. The refresh family is
      // untouched — and was not even rotated, since the refusal precedes the
      // mint — so the session survives and the caller degrades.
      throw new ResourceRefusedError(resource);
    }
    if (result.status === 400 || result.status === 401) {
      // Already on the refresh queue; queueing through clear() would deadlock.
      await this.clearNow();
      throw new SignedOutError();
    }
    if (result.status < 200 || result.status >= 300) {
      throw new Error(`Token refresh failed (HTTP ${result.status})`);
    }
    return parseTokenResponse(result.json);
  }

  private async persist(): Promise<void> {
    if (!this.record) {
      await this.storage.deleteItem(this.storageKey);
      return;
    }
    // Short-lived access tokens stay in memory only: they expire in minutes,
    // and expo-secure-store caps values near 2KB on Android.
    await this.storage.setItem(
      this.storageKey,
      JSON.stringify({ ...this.record, accessTokens: [] }),
    );
  }
}

/** The OAuth error code in a token endpoint's body, when it returned one. */
export function errorCode(json: unknown): string | null {
  if (!json || typeof json !== "object") {
    return null;
  }
  const code = (json as Record<string, unknown>).error;
  return typeof code === "string" ? code : null;
}

export function parseTokenResponse(json: unknown): TokenResponse {
  if (!json || typeof json !== "object") {
    throw new Error("Token response was not an object");
  }
  const body = json as Record<string, unknown>;
  const access = body.access_token;
  const refresh = body.refresh_token;
  const expires = body.expires_in;
  const type = body.token_type;
  const scope = body.scope;
  if (
    typeof access !== "string" ||
    typeof refresh !== "string" ||
    typeof expires !== "number" ||
    typeof type !== "string"
  ) {
    throw new Error("Token response was missing required fields");
  }
  return {
    access_token: access,
    refresh_token: refresh,
    expires_in: expires,
    token_type: type,
    ...(typeof scope === "string" ? { scope } : {}),
  };
}
