import { createPkcePair } from "./pkce";
import { randomUrlSafe } from "./crypto";
import { fetchRefusingRedirects } from "./http";
import {
  CLIENT_ID,
  OAUTH_SCOPE,
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
};

export function buildAuthorizeRequest(
  gatewayUrl: string,
  redirectUri = PRODUCTION_REDIRECT_URI,
): AuthorizeRequest {
  const base = validatedBaseUrl(gatewayUrl);
  const pkce = createPkcePair();
  const state = randomUrlSafe(24);
  const url = new URL(`${base}/oauth/authorize`);
  url.searchParams.set("response_type", "code");
  url.searchParams.set("client_id", CLIENT_ID);
  url.searchParams.set("redirect_uri", redirectUri);
  url.searchParams.set("scope", OAUTH_SCOPE);
  url.searchParams.set("state", state);
  url.searchParams.set("code_challenge", pkce.challenge);
  url.searchParams.set("code_challenge_method", "S256");
  return {
    authorizationUrl: url.toString(),
    redirectUri,
    state,
    verifier: pkce.verifier,
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
  fetchImpl: typeof fetch = fetch,
): Promise<GatewayMeta> {
  const base = validatedBaseUrl(gatewayUrl);
  const response = await fetchRefusingRedirects(
    fetchImpl,
    `${base}/api/v1/meta`,
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
  fetchImpl: typeof fetch = fetch,
): Promise<TokenResponse> {
  const base = validatedBaseUrl(gatewayUrl);
  const body = new URLSearchParams({
    grant_type: "authorization_code",
    code: params.code,
    code_verifier: params.verifier,
    client_id: CLIENT_ID,
    redirect_uri: params.redirectUri,
  });
  const response = await fetchRefusingRedirects(fetchImpl, `${base}/oauth/token`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: body.toString(),
  });
  if (!response.ok) {
    throw new Error(`Token exchange failed (HTTP ${response.status})`);
  }
  return parseTokenResponse(await response.json());
}

export async function fetchIdentity(
  gatewayUrl: string,
  accessToken: string,
  fetchImpl: typeof fetch = fetch,
): Promise<GatewayIdentity> {
  const base = validatedBaseUrl(gatewayUrl);
  const response = await fetchRefusingRedirects(fetchImpl, `${base}/api/v1/cli/me`, {
    headers: { Authorization: `Bearer ${accessToken}` },
  });
  if (!response.ok) {
    throw new Error(`Identity request failed (HTTP ${response.status})`);
  }
  return (await response.json()) as GatewayIdentity;
}

export function fetchTokenHttp(fetchImpl: typeof fetch = fetch) {
  return {
    async postForm(url: string, body: Record<string, string>) {
      const response = await fetchRefusingRedirects(fetchImpl, url, {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams(body).toString(),
      });
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
