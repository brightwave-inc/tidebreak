/**
 * Static machine tokens, and the QR payload that carries one.
 *
 * A standalone Tidebreak machine (`TIDEBREAK_AUTH_TOKENS_FILE`) authenticates
 * with long-lived bearer tokens from an operator-maintained roster, not with a
 * minted OAuth resource token. There is no authorization server in the path,
 * so nothing about the credential is negotiated: the operator hands one over
 * and the phone holds it until it is revoked.
 *
 * That makes the *shape* of the credential the only thing this app can check
 * before it puts one on the wire, and the server's rules are exact
 * (`crates/tidebreak-server/src/auth.rs`, `TokenMap`): at least 32 characters
 * drawn from `[A-Za-z0-9._~-]`, because the same string has to survive both an
 * `Authorization` header and the `tidebreak-token.<token>` WebSocket
 * subprotocol. Checking it here turns a mistyped paste into a message on the
 * attach screen instead of a 401 from a machine the user cannot reach.
 *
 * # The QR payload
 *
 * ```text
 * tidebreak-machine://v1?url=<percent-encoded https base URL>&token=<token>
 * ```
 *
 * Two properties of that string are deliberate and load-bearing.
 *
 * **It is a credential, so it is camera-only.** Unlike the gateway's provision
 * code (`provision.ts`), which carries a claimable handle that a person still
 * has to approve on a console, this payload is the whole secret. The scheme is
 * therefore one this app does **not** register in `app.json`: the OS will never
 * route `tidebreak-machine://` to the app as a deep link, so a web page cannot
 * hand the phone a machine and a token the user never scanned. `parseMachineQr`
 * is reachable only from the scanner the user opened.
 *
 * **It never becomes a route parameter.** The attach screen hosts its own
 * scanner rather than routing through `/scan`, so the token stays in component
 * state and never enters the router's history. Nothing in this module logs,
 * stringifies, or reflects a token into an error message.
 *
 * `v1` is the payload version, carried as the URL's host so a later revision
 * is a different string rather than a silently-reinterpreted one.
 */

import { UrlValidationError, validatedBaseUrl } from "./url";

/** The scheme, deliberately absent from this app's registered deep links. */
export const MACHINE_QR_SCHEME = "tidebreak-machine:";

/** The payload version, carried as the URL host. */
export const MACHINE_QR_VERSION = "v1";

/**
 * The roster's own floor (`MIN_TOKEN_LEN` in `auth.rs`). A machine refuses to
 * load a file with a shorter token, so one presented here cannot authenticate.
 */
export const MACHINE_TOKEN_MIN_LENGTH = 32;

/**
 * No server-side maximum exists for a roster token, but a header has to carry
 * it. This bounds what a scan or a paste can push into one, well above the
 * 64-character `openssl rand -hex 32` the docs recommend.
 */
export const MACHINE_TOKEN_MAX_LENGTH = 512;

/** A QR frame is small; there is no reason to parse a megabyte of one. */
const MAX_PAYLOAD_LENGTH = 2048;

/** The header- and subprotocol-safe alphabet `auth.rs` accepts. */
const TOKEN_PATTERN = /^[A-Za-z0-9._~-]+$/;

/**
 * Whether a string could be a roster token at all.
 *
 * A shape check, never an authenticity check: only the machine can say whether
 * a well-formed token names anybody.
 */
export function isMachineToken(value: string): boolean {
  return (
    value.length >= MACHINE_TOKEN_MIN_LENGTH &&
    value.length <= MACHINE_TOKEN_MAX_LENGTH &&
    TOKEN_PATTERN.test(value)
  );
}

/**
 * Why a pasted token cannot be used, or null when its shape is usable.
 *
 * Returns a reason rather than a sentence so the caller owns the copy and the
 * rule stays testable. No branch echoes the value it rejected.
 */
export function machineTokenProblem(
  raw: string,
): "empty" | "too_short" | "too_long" | "charset" | null {
  const token = raw.trim();
  if (token.length === 0) {
    return "empty";
  }
  if (token.length < MACHINE_TOKEN_MIN_LENGTH) {
    return "too_short";
  }
  if (token.length > MACHINE_TOKEN_MAX_LENGTH) {
    return "too_long";
  }
  return TOKEN_PATTERN.test(token) ? null : "charset";
}

export type MachineQrPayload = {
  /** Validated, trailing-slash-free machine base URL. */
  baseUrl: string;
  /** A shape-checked roster token. Never logged, never routed. */
  token: string;
};

/**
 * Reads a scanned payload, or returns null when it is not a well-formed
 * standalone attach code.
 *
 * Every rejection is silent and uniform: a scanner fires per camera frame, and
 * a payload that failed for an interesting reason is still just "not one of
 * ours" to the person holding the phone.
 */
export function parseMachineQr(data: string): MachineQrPayload | null {
  const text = data.trim();
  if (text.length === 0 || text.length > MAX_PAYLOAD_LENGTH) {
    return null;
  }
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if (url.protocol.toLowerCase() !== MACHINE_QR_SCHEME) {
    return null;
  }
  if (url.hostname.toLowerCase() !== MACHINE_QR_VERSION) {
    return null;
  }
  const token = url.searchParams.get("token") ?? "";
  if (!isMachineToken(token)) {
    return null;
  }
  const baseUrl = safeBaseUrl(url.searchParams.get("url") ?? "");
  if (!baseUrl) {
    return null;
  }
  return { baseUrl, token };
}

/**
 * Renders a payload, so an operator tool and the tests agree on one string.
 *
 * Exported for the round trip a test can assert rather than restate; nothing
 * in the app builds a payload, because the app only ever reads one.
 */
export function formatMachineQr(payload: MachineQrPayload): string {
  const params = new URLSearchParams({
    url: payload.baseUrl,
    token: payload.token,
  });
  return `${MACHINE_QR_SCHEME}//${MACHINE_QR_VERSION}?${params.toString()}`;
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
