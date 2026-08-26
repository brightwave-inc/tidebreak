export type TokenResponse = {
  access_token: string;
  refresh_token: string;
  expires_in: number;
  token_type: string;
};

export type CachedAccessToken = {
  resource: string;
  accessToken: string;
  expiresAtMs: number;
};

export type PersistedSession = {
  gatewayUrl: string;
  refreshToken: string;
  installationId?: string;
  machinePrefillUrl?: string;
  machine?: AttachedMachine;
  identity?: GatewayIdentity;
  accessTokens: CachedAccessToken[];
};

export type AttachedMachine = {
  baseUrl: string;
  resource: string;
};

export type GatewayMeta = {
  api_version?: string;
  installation_id?: string;
  gateway_version?: string;
  public_url?: string;
  auth_mode?: string;
  tidebreak_machine_url?: string | null;
};

export type GatewayIdentity = {
  user_id: string;
  email?: string | null;
  display_name?: string | null;
  session_id?: string;
  installation_id?: string;
};

export type AuthDiscovery = {
  mode: string;
  gateway_url?: string;
  resource?: string;
};

export type CodeWorkspaceStub = {
  id: string;
  title?: string | null;
  name?: string | null;
};

export const CLIENT_ID = "tidebreak-mobile";
export const OAUTH_SCOPE = "openid profile offline_access";
export const EXPIRY_LEEWAY_MS = 60_000;
export const PRODUCTION_REDIRECT_URI = "tidebreak://callback";
