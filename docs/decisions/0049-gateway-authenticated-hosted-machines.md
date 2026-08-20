# 49. Gateway-authenticated hosted machines

- Status: Proposed
- Date: 2026-08-19
- Owners: server, desktop
- Related: [`0047-gateway-linked-hosting.md`](0047-gateway-linked-hosting.md),
  [`0006-self-host-deployment-plane-authorization.md`](0006-self-host-deployment-plane-authorization.md),
  [`../gateway-boundary.md`](../gateway-boundary.md)
- Supersedes: decision 47's provisional roster/token-file identity mechanism

## Context

Decision 47 made the token file a bridge, not the destination. Generating one
standing Tidebreak token for every Model Gateway user leaves two identity
systems to operate, delays revocation until another provisioning run, and
requires a separate credential delivery channel. Terraform cannot repair that
without placing user secrets in state.

Tidebreak desktop already owns the stronger primitive: an authorization-code +
PKCE Model Gateway session with rotating refresh tokens and resource-bound
access tokens. The server already has a single credential-to-principal seam,
and all owner-scoped code-mode storage keys derive from that principal.

## Decision

1. A self-host server selects exactly one principal authenticator at boot:
   `TIDEBREAK_AUTH_GATEWAY_URL` for hosted Gateway identity, or
   `TIDEBREAK_AUTH_TOKENS_FILE` for standalone compatibility. Both or neither
   is a boot error before the shared store opens. An optional
   `TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL` changes only the server-to-server
   principal lookup route; discovery continues to expose the public identity
   URL so the desktop can match its existing OAuth session.
2. Gateway mode accepts only `mg_at_` access tokens and resolves them through
   `GET /api/v1/tidebreak/principal` at the configured Gateway. The response's
   stable user UUID maps to Tidebreak `user:<uuid>` owner identity; its live
   administrator bit maps to Tidebreak's deployment-plane role.
3. Gateway refusal, timeout, non-success response, oversized body, or malformed
   identity admits nobody. The server never falls back to its per-launch token,
   a shared deployment credential, email identity, or static tokens.
4. A hosted machine exposes public, non-secret `/auth/discovery` metadata naming
   its authentication mode, Gateway URL, and `tidebreak` resource. The desktop
   uses this only to select the authority it is already signed into.
5. “Connect with Model Gateway” mints the `tidebreak` resource from the
   desktop's existing OAuth session, probes the machine, and persists only the
   machine and Gateway URLs. The shell refreshes that token periodically from
   the rotating session; HTTP calls and new WebSocket handshakes use the latest
   value.
6. Static-token attachment remains available for ordinary self-host servers.
   Its existing token-file format and role semantics are unchanged.

## Consequences

- A person who can use Model Gateway can use the hosted Tidebreak machine with
  the same account; no Tidebreak account or copied token is provisioned.
- Gateway deactivation, session revocation, and role changes are enforced by
  the next validation request.
- Gateway availability is now on the authentication path for its hosted
  Tidebreak machine. The dependency is deliberately fail closed.
- WebSocket authentication still uses `Sec-WebSocket-Protocol`; operators must
  exclude it from proxy logs even though the bearer is now short-lived.
- The local-laptop authority boundary is unchanged. A remote hosted machine
  does not gain access to the desktop's folders, input, screen, or native
  executor.

## Validation

- Member and administrator Gateway projections map to distinct Tidebreak roles
  while preserving the same stable UUID owner key.
- Revoked, inactive, wrong-resource, malformed, and Gateway-unreachable cases
  all return `401` from the Tidebreak API.
- The desktop stores no Gateway resource token for a Gateway-backed attachment
  and refreshes before access-token expiry.
- Static-token self-host tests continue to pass unchanged.
