# 51. On-behalf-of inference for hosted machines

- Status: Proposed
- Date: 2026-08-20
- Owners: server
- Related: [`0047-gateway-linked-hosting.md`](0047-gateway-linked-hosting.md),
  [`0049-gateway-authenticated-hosted-machines.md`](0049-gateway-authenticated-hosted-machines.md),
  [`../gateway-boundary.md`](../gateway-boundary.md)
- Supersedes: the deployment-scoped inference credential as the default for
  gateway-authenticated hosted machines

## Context

Decision 49 gave hosted machines live gateway identity: every request carries
the caller's short-lived, machine-bound gateway token, and the server retains
that bearer per connection to revalidate identity. Inference still rides a
different, weaker credential. The hosting deployment injects one
inference-only gateway token through the environment, so every turn any user
drives is attributed to the deployment, the token is a standing secret in a
Secret store, and rotation is an operator chore.

The gateway side is adding a token exchange for exactly this shape: a hosted
machine may exchange a caller's live machine-bound token for a short-lived,
inference-only token for the same user. This record adopts that exchange as
the server's default inference path when gateway authentication is enabled.

## Decision

1. When the server runs with gateway authentication (decision 49) and no
   stored provider configuration overrides it, the server resolves model
   credentials per caller: it exchanges the caller's presented machine-bound
   token at the configured gateway for a short-lived inference token, and
   drives that caller's turns with it.
2. Exchanged tokens live in process memory only, keyed by user, and are
   minted again near expiry. The server never persists them and never issues
   them to clients.
3. A stored provider configuration always wins, and the environment-variable
   fallbacks for provider credentials and base URLs keep their existing
   semantics. On-behalf-of is the default only when the deployment states no
   other inference path.
4. Fail closed. If the gateway refuses an exchange mid-session — revoked
   session, deactivated user, expired token the client failed to refresh —
   the turn fails with the same sign-in-required shape the client already
   handles. The server never falls back from a per-user credential to a
   shared one.
5. Static-token servers are unchanged. Without gateway authentication there
   is no machine-bound token to exchange, so those deployments keep
   environment-configured providers.
6. The desktop is unchanged. It already refreshes the machine-bound token the
   exchange consumes.

## Consequences

- A hosting deployment needs no inference secret: no minted token, no Secret,
  no rotation. Gateway-side usage attribution becomes per-user.
- A caller's model access now ends exactly when their gateway access ends,
  mid-session, rather than when an operator rotates a shared token.
- The gateway is already on the authentication path for every request
  (decision 49), so the exchange adds no new availability coupling class,
  but it does add a gateway round-trip when a cached token nears expiry.
- A machine-bound token becomes exchangeable for inference as its user for
  the token's remaining lifetime. The token is short-lived, replays against
  no other machine, and the exchanged token is inference-only, which is the
  same authority the user's own client already holds.

## Validation

- A turn driven by a member and a turn driven by an administrator meter to
  those users at the gateway, not to the deployment.
- Revoking a user's gateway session fails their next turn closed; other
  users' turns continue.
- A deployment with a stored provider configuration, and a static-token
  deployment with environment-configured providers, behave exactly as before
  this record.
- The exchanged token never appears in the store, in logs, or in any client
  response.
