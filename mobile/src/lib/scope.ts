/**
 * What authority this client asks a gateway for, and what a session got.
 *
 * Two rules shape everything here.
 *
 * A gateway's authorization page refuses a scope it does not know rather than
 * ignoring it (`canonical_scope`, mg `cli_auth.rs`), and refuses control-plane
 * scopes outright from a client whose registration is not control-plane
 * capable. Either refusal breaks sign-in completely — not the console surface,
 * the whole pairing. So a scope is requested only from a gateway that
 * advertises it on `GET /api/v1/meta`, and the absence of an advertisement
 * always means "do not ask".
 *
 * The advertisement that matters is `surfaces.tidebreak_mobile_console`: this
 * installation's `tidebreak-mobile` client may hold the console resources
 * (brightwave-inc/model-gateway#2044). `surfaces.control_plane_write` is *not*
 * a substitute — it is a binary-wide fact about the scope vocabulary, true on
 * gateways deployed today whose `tidebreak-mobile` client is still confined to
 * `control` + `tidebreak:*`. Gating on it alone would break sign-in against
 * every one of them.
 */

import type { GatewayMeta, TokenResponse } from "./types";

/** Pairing and machine supervision: what every session has always asked for. */
export const BASELINE_SCOPE = "openid profile offline_access";

/** The versioned admin/observability reads (gateway console). */
export const CONTROL_PLANE_READ_SCOPE = "control_plane:read";

/** Control-plane mutations; still role-checked live on every request. */
export const CONTROL_PLANE_WRITE_SCOPE = "control_plane:write";

/** The owner-scoped sandbox verbs. */
export const RUNTIME_EXECUTE_SCOPE = "runtime:execute";

/**
 * The scope to request at `/oauth/authorize` for one gateway.
 *
 * Meta is read unauthenticated before the browser opens, so this is decided
 * from a fact already on hand rather than probed a second time: a redundant
 * probe would stall the sheet and, on a transient failure, silently downgrade
 * a console-capable gateway to a machine-only session.
 */
export function requestedScope(meta: GatewayMeta | null | undefined): string {
  if (meta?.surfaces?.tidebreak_mobile_console !== true) {
    return BASELINE_SCOPE;
  }
  const scopes = [
    BASELINE_SCOPE,
    CONTROL_PLANE_READ_SCOPE,
    RUNTIME_EXECUTE_SCOPE,
  ];
  if (meta.surfaces.control_plane_write === true) {
    scopes.push(CONTROL_PLANE_WRITE_SCOPE);
  }
  return scopes.join(" ");
}

/** Whether a recorded grant carries one scope token. */
export function scopeGrants(
  granted: string | undefined | null,
  scope: string,
): boolean {
  if (!granted) {
    return false;
  }
  return granted.split(/\s+/).includes(scope);
}

/**
 * The grant to record for a new session, taken from the authorization-code
 * exchange — the one response that answers about the session rather than about
 * a resource.
 *
 * Only what the gateway actually returned is recorded. RFC 6749 §5.1 lets a
 * server omit `scope` to mean "identical to the scope requested", and reading
 * the omission that way would be defensible in general; it is not defensible
 * here, because this value gates console and runtime surfaces. Recording the
 * request would let a gateway that silently narrowed (or never understood) the
 * request light up authority it never granted. Unknown reads as baseline
 * instead. Model Gateway's token endpoint always returns `scope`, so a real
 * pairing records its true grant and loses nothing to this conservatism.
 */
export function grantedScopeFrom(
  tokens: Pick<TokenResponse, "scope">,
): string | undefined {
  const granted = tokens.scope?.trim();
  return granted ? granted : undefined;
}

/**
 * Whether this session may read the gateway console.
 *
 * Asked of the grant the session actually holds, never of a constant: a
 * session signed in before an operator widened the gateway keeps the authority
 * it consented to until the user signs in again. A session whose grant was
 * never recorded reads as no authority — unknown authority is not authority.
 */
export function grantsConsoleRead(granted: string | undefined): boolean {
  return scopeGrants(granted, CONTROL_PLANE_READ_SCOPE);
}

/** Whether this session may mutate the control plane (mg ADR 0102). */
export function grantsConsoleWrite(granted: string | undefined): boolean {
  return scopeGrants(granted, CONTROL_PLANE_WRITE_SCOPE);
}

/** Whether this session may drive the runtime sandbox verbs. */
export function grantsRuntimeExecute(granted: string | undefined): boolean {
  return scopeGrants(granted, RUNTIME_EXECUTE_SCOPE);
}
