import {
  CLIENT_ID,
  EXPIRY_LEEWAY_MS,
  type CachedAccessToken,
  type PersistedSession,
  type TokenResponse,
} from "./types";
import { SESSION_STORAGE_KEY, type SecureStorage } from "./storage";
import { isAllowedResource } from "./resource";

export class SignedOutError extends Error {
  constructor(message = "The gateway session is no longer valid") {
    super(message);
    this.name = "SignedOutError";
  }
}

export type TokenHttp = {
  postForm(
    url: string,
    body: Record<string, string>,
  ): Promise<{ status: number; json: unknown }>;
};

export class TokenStore {
  private session: PersistedSession | null = null;
  private refreshTail: Promise<void> = Promise.resolve();
  private inflight = new Map<string, Promise<string>>();
  private signedOutListeners = new Set<() => void>();

  constructor(
    private readonly storage: SecureStorage,
    private readonly http: TokenHttp,
  ) {}

  onSignedOut(listener: () => void): () => void {
    this.signedOutListeners.add(listener);
    return () => {
      this.signedOutListeners.delete(listener);
    };
  }

  async hydrate(): Promise<PersistedSession | null> {
    const raw = await this.storage.getItem(SESSION_STORAGE_KEY);
    if (!raw) {
      this.session = null;
      return null;
    }
    try {
      this.session = JSON.parse(raw) as PersistedSession;
    } catch {
      await this.clear();
      return null;
    }
    return this.session;
  }

  snapshot(): PersistedSession | null {
    return this.session;
  }

  async replace(session: PersistedSession): Promise<void> {
    this.session = session;
    await this.persist();
  }

  async update(partial: Partial<PersistedSession>): Promise<void> {
    if (!this.session) {
      throw new SignedOutError();
    }
    this.session = { ...this.session, ...partial };
    await this.persist();
  }

  async clear(): Promise<void> {
    this.session = null;
    this.inflight.clear();
    await this.storage.deleteItem(SESSION_STORAGE_KEY);
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
    const hit = this.session?.accessTokens.find(
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
    const session = this.session;
    if (!session) {
      throw new SignedOutError();
    }
    const tokens = await this.requestRefresh(session, resource);
    const cached: CachedAccessToken = {
      resource,
      accessToken: tokens.access_token,
      expiresAtMs: Date.now() + tokens.expires_in * 1000,
    };
    const others = session.accessTokens.filter(
      (token) => token.resource !== resource,
    );
    this.session = {
      ...session,
      refreshToken: tokens.refresh_token,
      accessTokens: [...others, cached],
    };
    await this.persist();
    return tokens.access_token;
  }

  private async requestRefresh(
    session: PersistedSession,
    resource: string,
  ): Promise<TokenResponse> {
    const result = await this.http.postForm(`${session.gatewayUrl}/oauth/token`, {
      grant_type: "refresh_token",
      refresh_token: session.refreshToken,
      client_id: CLIENT_ID,
      resource,
    });
    if (result.status === 400 || result.status === 401) {
      await this.clear();
      throw new SignedOutError();
    }
    if (result.status < 200 || result.status >= 300) {
      throw new Error(`Token refresh failed (HTTP ${result.status})`);
    }
    return parseTokenResponse(result.json);
  }

  private async persist(): Promise<void> {
    if (!this.session) {
      await this.storage.deleteItem(SESSION_STORAGE_KEY);
      return;
    }
    await this.storage.setItem(
      SESSION_STORAGE_KEY,
      JSON.stringify(this.session),
    );
  }
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
  };
}
