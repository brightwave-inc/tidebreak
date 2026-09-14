/**
 * QR / console pairing: the phone's half of mg ADR 0086.
 *
 * The console shows a QR carrying a gateway URL and a pairing-session handle.
 * The handle is not a credential — this phone claims it with a fresh PKCE
 * challenge, both screens show the same short match code, the person at the
 * console approves the claim, and only then does a single poll receive an
 * authorization code. That code is redeemed through the ordinary
 * authorization-code grant (`gateway.ts`), so QR pairing lands in exactly the
 * connection the browser flow produces — same client, same redirect, same
 * rotating refresh family.
 *
 * Two details are load-bearing and easy to get wrong:
 *
 * **`client_id`.** The session is created before any phone has claimed it, so
 * it names no client; the claim and the poll each declare one
 * (brightwave-inc/model-gateway#2045). Omitting it means `tidewatch`, whose
 * registered redirect schemes are not this app's — the claim would be accepted
 * and the redemption would then fail on the redirect match. Every request here
 * sends `tidebreak-mobile`.
 *
 * **The challenge accompanies the poll.** The session code is printed in the
 * QR and is therefore public; the challenge never is. Sending it on each poll
 * is what entitles this phone, and only this phone, to collect the code.
 */

import { sha256Bytes } from "./crypto";
import { fetchRefusingRedirects, type HttpFetch } from "./http";
import { CLIENT_ID } from "./types";
import { validatedBaseUrl } from "./url";

/**
 * The user-code alphabet, free of the characters people misread (I/O/0/1).
 * Must stay byte-identical to the server's (`derive_pairing_match_code`,
 * mg `cli_auth.rs`) or the two screens show codes that disagree.
 */
const MATCH_CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/** The prefix the gateway gives a pairing-session handle. */
export const PAIRING_SESSION_PREFIX = "mg_ps_";

/** Seconds the console leaves a claim standing; the poll cadence it asks for. */
export const PAIRING_POLL_INTERVAL_MS = 2000;

/**
 * The short code both screens display: the first 20 bits of
 * SHA-256(challenge), five bits per character, `XX-XX`.
 *
 * Synchronous, unlike the reference app's — this codebase hashes with
 * `js-sha256` and reads the digest bytes directly, so there is no promise and
 * no base64 round-trip through `atob` (which Hermes may not provide).
 */
export function deriveMatchCode(challenge: string): string {
  const digest = sha256Bytes(challenge);
  const bits =
    ((digest[0] ?? 0) << 16) | ((digest[1] ?? 0) << 8) | (digest[2] ?? 0);
  const chars = [0, 1, 2, 3].map(
    (index) => MATCH_CODE_ALPHABET[(bits >> (19 - 5 * index)) & 0x1f] ?? "",
  );
  return `${chars[0]}${chars[1]}-${chars[2]}${chars[3]}`;
}

/**
 * A pairing endpoint's refusal, carrying the OAuth error code. The code is the
 * whole point: `authorization_pending` is the normal case on the poll loop and
 * must not read as a failure.
 */
export class PairingError extends Error {
  readonly code: string | null;
  readonly status: number;

  constructor(message: string, status: number, code: string | null) {
    super(message);
    this.name = "PairingError";
    this.status = status;
    this.code = code;
  }
}

export function isPairingPending(error: unknown): boolean {
  return (
    error instanceof PairingError && error.code === "authorization_pending"
  );
}

/** What to tell the user about a pairing failure. */
export function pairingErrorMessage(error: unknown): string {
  if (error instanceof PairingError) {
    switch (error.code) {
      case "access_denied":
        return "The gateway console denied this pairing request.";
      case "expired_token":
      case "invalid_grant":
        return "The pairing session is no longer valid — show a fresh code on the console and scan again.";
      default:
        return error.message;
    }
  }
  return error instanceof Error ? error.message : "Pairing failed.";
}

async function pairingRequest(
  gatewayUrl: string,
  path: string,
  form: Record<string, string>,
  fetchImpl?: HttpFetch,
): Promise<unknown> {
  const base = validatedBaseUrl(gatewayUrl);
  const response = await fetchRefusingRedirects(
    `${base}${path}`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/x-www-form-urlencoded",
        Accept: "application/json",
      },
      body: new URLSearchParams(form).toString(),
    },
    fetchImpl,
  );
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    body = null;
  }
  if (!response.ok) {
    const fields =
      body && typeof body === "object"
        ? (body as Record<string, unknown>)
        : {};
    const code = typeof fields.error === "string" ? fields.error : null;
    const description =
      typeof fields.error_description === "string"
        ? fields.error_description
        : null;
    throw new PairingError(
      description ?? code ?? `Pairing request failed (HTTP ${response.status})`,
      response.status,
      code,
    );
  }
  return body;
}

/**
 * Claims the scanned session for this phone. First claim wins; a second one
 * is refused, so a photographed QR cannot be raced after the fact.
 */
export async function claimPairingSession(
  gatewayUrl: string,
  params: {
    sessionCode: string;
    codeChallenge: string;
    redirectUri: string;
    deviceLabel?: string;
  },
  fetchImpl?: HttpFetch,
): Promise<void> {
  await pairingRequest(
    gatewayUrl,
    "/oauth/pairing/claim",
    {
      session_code: params.sessionCode,
      code_challenge: params.codeChallenge,
      redirect_uri: params.redirectUri,
      client_id: CLIENT_ID,
      ...(params.deviceLabel ? { device_label: params.deviceLabel } : {}),
    },
    fetchImpl,
  );
}

/**
 * One poll for the console's decision. Resolves with the authorization code
 * once approved; throws `PairingError` with `authorization_pending` while the
 * console has not decided, `access_denied` on deny, `expired_token` when the
 * session lapsed.
 */
export async function pollPairingSession(
  gatewayUrl: string,
  sessionCode: string,
  codeChallenge: string,
  fetchImpl?: HttpFetch,
): Promise<string> {
  const body = await pairingRequest(
    gatewayUrl,
    "/oauth/pairing/token",
    {
      session_code: sessionCode,
      code_challenge: codeChallenge,
      client_id: CLIENT_ID,
    },
    fetchImpl,
  );
  const code =
    body && typeof body === "object"
      ? (body as Record<string, unknown>).code
      : null;
  if (typeof code !== "string" || code.length === 0) {
    throw new PairingError(
      "The gateway approved this phone but returned no authorization code.",
      200,
      null,
    );
  }
  return code;
}

export type PairingApproval =
  | { kind: "approved"; code: string }
  | { kind: "cancelled" };

/**
 * The poll loop, as a pure state machine over injected effects.
 *
 * Its only interesting behaviour is what it refuses to do. A poll that
 * resolves *after* the user cancelled must not sign them in behind their back,
 * so the cancel flag is re-read once the code is already in hand — the claim
 * simply stays live server-side until the console denies it or it expires.
 * Everything else is a straight loop: pending retries, any other refusal
 * throws.
 */
export async function awaitPairingApproval(deps: {
  poll: () => Promise<string>;
  sleep: (ms: number) => Promise<void>;
  cancelled: () => boolean;
  intervalMs?: number;
}): Promise<PairingApproval> {
  const interval = deps.intervalMs ?? PAIRING_POLL_INTERVAL_MS;
  while (!deps.cancelled()) {
    await deps.sleep(interval);
    if (deps.cancelled()) {
      return { kind: "cancelled" };
    }
    try {
      const code = await deps.poll();
      if (deps.cancelled()) {
        return { kind: "cancelled" };
      }
      return { kind: "approved", code };
    } catch (error) {
      if (isPairingPending(error)) {
        continue;
      }
      throw error;
    }
  }
  return { kind: "cancelled" };
}
