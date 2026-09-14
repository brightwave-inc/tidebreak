export type TokenResponse = {
  access_token: string;
  refresh_token: string;
  expires_in: number;
  token_type: string;
  /**
   * The scope the gateway granted. Present on the authorization-code exchange,
   * where it describes the session; a refresh response describes only the
   * resource it minted, so the session grant is carried rather than re-read.
   * Optional because an older gateway may omit it, and a missing grant reads
   * as "no console authority" rather than as a parse failure.
   */
  scope?: string;
};

export type CachedAccessToken = {
  resource: string;
  accessToken: string;
  expiresAtMs: number;
};

/**
 * The single-session blob builds before the connection model wrote under
 * `tidebreak.mobile.session.v1`. Read once at hydrate and migrated into a
 * gateway connection; nothing writes this shape any more.
 */
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
  /** Capability advertisement; absent on an older gateway (see `scope.ts`). */
  surfaces?: GatewaySurfaces;
};

/**
 * What one installation says it supports. Every field is optional and absent
 * means "no": a client must never request an authority the gateway has not
 * advertised, because the authorization server refuses a scope it does not
 * know outright rather than ignoring it.
 */
export type GatewaySurfaces = {
  /**
   * Whether this installation delivers mobile push notifications (mg ADR
   * 0093). A client must not offer push registration when this is absent: a
   * dark installation answers the device and preference routes with 404.
   */
  push?: boolean;
  /**
   * Whether this installation's `tidebreak-mobile` client may hold the gateway
   * console resources (`control_plane`, `runtime:<slug>`) — the widening in
   * brightwave-inc/model-gateway#2044. Absent on every gateway deployed today.
   */
  tidebreak_mobile_console?: boolean;
  /**
   * Whether the authorization server accepts the `control_plane:write` scope
   * (mg ADR 0102). Binary-wide, so it says nothing on its own about whether
   * this client may hold control-plane resources at all.
   */
  control_plane_write?: boolean;
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
export const EXPIRY_LEEWAY_MS = 60_000;
export const PRODUCTION_REDIRECT_URI = "tidebreak://callback";
