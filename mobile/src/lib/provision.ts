/**
 * What a scanned QR — or an inbound `tidebreak://provision` deep link — is
 * allowed to mean.
 *
 * The gateway console renders a provision link carrying its public URL and a
 * pairing-session handle. The handle is deliberately not a credential: this
 * phone claims it with a PKCE challenge and a person at the console approves
 * the claim (mg ADR 0086), so the payload is safe on any channel, including
 * somebody else's camera. What still needs guarding is what the app *does*
 * with a payload from an untrusted source, and that is entirely this module's
 * job:
 *
 * - **Scheme allowlist.** Only this app's own schemes are provision links.
 *   All three variants are accepted rather than just the running build's, so
 *   a staging phone can scan a production console's code — the payload is
 *   inert either way, and the gateway URL is re-validated before use.
 * - **Handle shape.** Anything that is not an `mg_ps_` session handle is
 *   dropped rather than passed on, so a crafted link cannot smuggle an
 *   arbitrary string into the claim form.
 * - **Gateway URL.** Returned only after `validatedBaseUrl` accepts it, which
 *   is what refuses `http://` to a non-loopback host, embedded credentials,
 *   and anything that is not a URL at all.
 *
 * Parsing uses `URL` rather than `expo-linking` so the rules above are a pure
 * function with no native module behind them.
 */

import { PAIRING_SESSION_PREFIX } from "./pairing";
import { UrlValidationError, validatedBaseUrl } from "./url";

/** This app's registered schemes, across all three build variants. */
const PROVISION_SCHEMES = new Set([
  "tidebreak:",
  "tidebreak-staging:",
  "tidebreak-dev:",
]);

/** The deep-link host that means "pair with this gateway". */
const PROVISION_HOST = "provision";

/**
 * A QR payload is a URL, and a hostile one is still only a URL — but there is
 * no reason to run the parser over a megabyte of it.
 */
const MAX_PAYLOAD_LENGTH = 2048;

export type PairingScan = {
  /** Validated, trailing-slash-free gateway base URL. */
  gatewayUrl: string;
  /** Claimable pairing-session handle, or null for a bare gateway URL. */
  sessionCode: string | null;
};

/**
 * Reads a scanned or deep-linked payload, or returns null when it is not one
 * of ours. A bare `https://` gateway URL is accepted and yields no session
 * handle: it pairs, and sign-in falls back to the browser.
 */
export function parsePairingScan(data: string): PairingScan | null {
  const text = data.trim();
  if (text.length === 0 || text.length > MAX_PAYLOAD_LENGTH) {
    return null;
  }
  if (/^https?:\/\//i.test(text)) {
    const gatewayUrl = safeBaseUrl(text);
    return gatewayUrl ? { gatewayUrl, sessionCode: null } : null;
  }
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if (!PROVISION_SCHEMES.has(url.protocol.toLowerCase())) {
    return null;
  }
  // `tidebreak://callback?code=…` reaches this module too — the OAuth redirect
  // and a provision link share a scheme, and only the host tells them apart.
  if (url.hostname.toLowerCase() !== PROVISION_HOST) {
    return null;
  }
  const gatewayUrl = safeBaseUrl(url.searchParams.get("gateway") ?? "");
  if (!gatewayUrl) {
    return null;
  }
  const session = url.searchParams.get("session") ?? "";
  return {
    gatewayUrl,
    sessionCode: isPairingSessionHandle(session) ? session : null,
  };
}

/** Whether a string is shaped like a gateway pairing-session handle. */
export function isPairingSessionHandle(value: string): boolean {
  return (
    value.startsWith(PAIRING_SESSION_PREFIX) &&
    value.length > PAIRING_SESSION_PREFIX.length &&
    /^[A-Za-z0-9_-]+$/.test(value)
  );
}

function safeBaseUrl(candidate: string): string | null {
  try {
    return validatedBaseUrl(candidate);
  } catch (error) {
    if (error instanceof UrlValidationError) {
      return null;
    }
    throw error;
  }
}
