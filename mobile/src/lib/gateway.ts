import { createPkcePair } from "./pkce";
import { randomUrlSafe } from "./crypto";
import { fetchRefusingRedirects, type HttpFetch } from "./http";
import { requestedScope } from "./scope";
import {
  CLIENT_ID,
  PRODUCTION_REDIRECT_URI,
  type GatewayIdentity,
  type GatewayMeta,
  type TokenResponse,
} from "./types";
import { parseTokenResponse } from "./tokenStore";
import { validatedBaseUrl } from "./url";

export type AuthorizeRequest = {
  authorizationUrl: string;
  redirectUri: string;
  state: string;
  verifier: string;
  /** The scope actually asked for, so the caller can record what was granted. */
  scope: string;
};

/**
 * `meta` is the unauthenticated metadata pairing already read. It decides the
 * scope: the gateway refuses a scope it does not advertise rather than
 * ignoring it, so anything not advertised is not requested (`scope.ts`).
 */
export function buildAuthorizeRequest(
  gatewayUrl: string,
  redirectUri = PRODUCTION_REDIRECT_URI,
  meta?: GatewayMeta | null,
): AuthorizeRequest {
  const base = validatedBaseUrl(gatewayUrl);
  const pkce = createPkcePair();
  const state = randomUrlSafe(24);
  const scope = requestedScope(meta);
  const url = new URL(`${base}/oauth/authorize`);
  url.searchParams.set("response_type", "code");
  url.searchParams.set("client_id", CLIENT_ID);
  url.searchParams.set("redirect_uri", redirectUri);
  url.searchParams.set("scope", scope);
  url.searchParams.set("state", state);
  url.searchParams.set("code_challenge", pkce.challenge);
  url.searchParams.set("code_challenge_method", "S256");
  return {
    authorizationUrl: url.toString(),
    redirectUri,
    state,
    verifier: pkce.verifier,
    scope,
  };
}

export function parseOAuthCallback(
  callbackUrl: string,
  expectedState: string,
): string {
  const url = new URL(callbackUrl);
  const error = url.searchParams.get("error");
  if (error) {
    throw new Error(url.searchParams.get("error_description") ?? error);
  }
  const state = url.searchParams.get("state");
  if (!state || state !== expectedState) {
    throw new Error("OAuth state did not match; refusing the callback.");
  }
  const code = url.searchParams.get("code");
  if (!code) {
    throw new Error("OAuth callback did not include an authorization code.");
  }
  return code;
}

export async function fetchGatewayMeta(
  gatewayUrl: string,
  fetchImpl?: HttpFetch,
): Promise<GatewayMeta> {
  const base = validatedBaseUrl(gatewayUrl);
  const response = await fetchRefusingRedirects(
    `${base}/api/v1/meta`,
    undefined,
    fetchImpl,
  );
  if (!response.ok) {
    throw new Error(`Gateway metadata request failed (HTTP ${response.status})`);
  }
  return (await response.json()) as GatewayMeta;
}

export async function exchangeAuthorizationCode(
  gatewayUrl: string,
  params: {
    code: string;
    verifier: string;
    redirectUri: string;
  },
  fetchImpl?: HttpFetch,
): Promise<TokenResponse> {
  const base = validatedBaseUrl(gatewayUrl);
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code: params.code,
    code_verifier: params.verifier,
    client_id: CLIENT_ID,
    redirect_uri: params.redirectUri,
  });
  const response = await fetchRefusingRedirects(
    `${base}/oauth/token`,
    {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: body.toString(),
    },
    fetchImpl,
  );
  if (!response.ok) {
    throw new Error(`Token exchange failed (HTTP ${response.status})`);
  }
  return parseTokenResponse(await response.json());
}

export async function fetchIdentity(
  gatewayUrl: string,
  accessToken: string,
  fetchImpl?: HttpFetch,
): Promise<GatewayIdentity> {
  const base = validatedBaseUrl(gatewayUrl);
  const response = await fetchRefusingRedirects(
    `${base}/api/v1/cli/me`,
    { headers: { Authorization: `Bearer ${accessToken}` } },
    fetchImpl,
  );
  if (!response.ok) {
    throw new Error(`Identity request failed (HTTP ${response.status})`);
  }
  return (await response.json()) as GatewayIdentity;
}

export function fetchTokenHttp(fetchImpl?: HttpFetch) {
  return {
    async postForm(url: string, body: Record<string, string>) {
      const response = await fetchRefusingRedirects(
        url,
        {
          method: "POST",
          headers: { "Content-Type": "application/x-www-form-urlencoded" },
          body: new URLSearchParams(body).toString(),
        },
        fetchImpl,
      );
      let json: unknown = null;
      try {
        json = await response.json();
      } catch {
        json = null;
      }
      return { status: response.status, json };
    },
  };
}
